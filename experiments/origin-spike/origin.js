// M0.1 origin spike, docs/stages/m0-spikes.md. Results go to the runtime stderr.
const token = new URLSearchParams(location.hash.slice(1)).get('capability') ?? '';
const auth = { Authorization: `Bearer ${token}` };
const HTTPS = location.protocol === 'https:';
const assetBase = HTTPS ? location.origin : 'native://app';
const logElement = document.getElementById('log');
const log = line => { logElement.textContent += `${line}\n`; };
const round = value => Math.round(value * 100) / 100;
const stats = values => {
  const sorted = [...values].sort((a, b) => a - b);
  const at = q => sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))];
  return { n: sorted.length, min: round(sorted[0]), p50: round(at(0.5)), p90: round(at(0.9)), max: round(sorted.at(-1)) };
};
const nowMs = () => (window.performance ? performance.now() : Date.now());

async function report(payload) {
  await fetch('native://spike/report', { method: 'POST', headers: auth, body: JSON.stringify(payload) });
}

function checkIndexedDB() {
  return new Promise(resolve => {
    if (!('indexedDB' in window)) { resolve('missing'); return; }
    let request;
    try { request = indexedDB.open('alef-origin-spike', 1); } catch (error) { resolve(`open threw: ${error}`); return; }
    request.onupgradeneeded = () => request.result.createObjectStore('kv');
    request.onerror = () => resolve(`open error: ${request.error}`);
    request.onsuccess = () => {
      const db = request.result;
      const tx = db.transaction('kv', 'readwrite');
      tx.objectStore('kv').put('v1', 'k1');
      tx.oncomplete = () => {
        const read = db.transaction('kv').objectStore('kv').get('k1');
        read.onsuccess = () => { resolve(read.result === 'v1' ? 'open+put+get ok' : `unexpected value: ${read.result}`); db.close(); };
        read.onerror = () => { resolve(`get error: ${read.error}`); db.close(); };
      };
      tx.onerror = () => { resolve(`put error: ${tx.error}`); db.close(); };
    };
  });
}

async function checkStorage() {
  const result = {};
  for (const name of ['localStorage', 'sessionStorage']) {
    try {
      const store = window[name];
      store.setItem('alef-spike', 'works');
      result[name] = store.getItem('alef-spike') === 'works' ? 'write+read ok' : 'unexpected';
      store.removeItem('alef-spike');
    } catch (error) { result[name] = `error: ${error.name}: ${error.message}`; }
  }
  result.indexedDB = await checkIndexedDB();
  return result;
}

async function checkCrypto() {
  try {
    const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode('alef'));
    const hex = [...new Uint8Array(digest)].slice(0, 4).map(b => b.toString(16).padStart(2, '0')).join('');
    return `ok sha256=${hex}...`;
  } catch (error) { return `error: ${error.name}: ${error.message}`; }
}

// Each ping must yield HTTP 200 and body "1" (sink echoes the request length).
async function ping50(count) {
  const samples = [];
  let failures = 0;
  let firstFailure = null;
  for (let i = 0; i < count; i += 1) {
    const started = nowMs();
    const response = await fetch('native://spike/sink', { method: 'POST', headers: auth, body: 'x' });
    if (response.status !== 200) { failures += 1; firstFailure = firstFailure ?? `HTTP ${response.status}`; continue; }
    const text = await response.text();
    if (text !== '1') { failures += 1; firstFailure = firstFailure ?? `body ${JSON.stringify(text)}`; continue; }
    samples.push(nowMs() - started);
  }
  return { samples, failures, firstFailure };
}

async function pingWithExtraHeader() {
  const started = nowMs();
  try {
    const response = await fetch('native://spike/sink', { method: 'POST', headers: { ...auth, 'X-Alef-Spike': '1' }, body: 'x' });
    if (response.status !== 200) return { ok: false, ms: round(nowMs() - started), error: `HTTP ${response.status}` };
    const text = await response.text();
    if (text !== '1') return { ok: false, ms: round(nowMs() - started), error: `body ${JSON.stringify(text)}` };
    return { ok: true, ms: round(nowMs() - started) };
  } catch (error) { return { ok: false, ms: round(nowMs() - started), error: String(error) }; }
}

async function checkInvoke() {
  const started = nowMs();
  const response = await fetch('native://invoke/', {
    method: 'POST', headers: { ...auth, 'Content-Type': 'application/json' },
    body: JSON.stringify({ command: 'runtime.window', arguments: { action: 'getState' } }),
  });
  const state = await response.json();
  return { ok: response.ok && typeof state.revision === 'number', ms: round(nowMs() - started), revision: state.revision };
}

const BIG_JS_BYTES = 2097157;

async function checkAsset() {
  const runs = [];
  for (let run = 0; run < 3; run += 1) {
    const started = nowMs();
    const response = await fetch(`${assetBase}/big.js?v=${run}`);
    const buffer = await response.arrayBuffer();
    runs.push({
      ms: round(nowMs() - started),
      bytes: buffer.byteLength,
      status: response.status,
      intact: response.ok && response.status === 200 && buffer.byteLength === BIG_JS_BYTES,
    });
  }
  return runs;
}

// Only a TypeError whose message mentions cancellation counts as intercepted;
// the 3 s AbortError timeout or any other failure is reported separately as FAILURE.
async function checkExternalBlock() {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 3000);
  const started = nowMs();
  try {
    await fetch('https://example.com/', { signal: controller.signal });
    clearTimeout(timer);
    return { blocked: false, kind: 'not blocked (FAILURE)', ms: round(nowMs() - started) };
  } catch (error) {
    clearTimeout(timer);
    const ms = round(nowMs() - started);
    if (error.name === 'TypeError' && error.message.toLowerCase().includes('cancel')) {
      return { blocked: true, kind: 'cancelled', ms };
    }
    const kind = error.name === 'AbortError'
      ? 'timeout-abort (FAILURE)'
      : `unexpected: ${error.name}: ${error.message}`;
    return { blocked: false, kind, ms };
  }
}

async function closeWindow() {
  await fetch('native://invoke/', {
    method: 'POST', headers: { ...auth, 'Content-Type': 'application/json' },
    body: JSON.stringify({ command: 'runtime.window', arguments: { action: 'close' } }),
  });
}

async function run() {
  const results = {
    mode: HTTPS ? 'https-intercepted' : 'native-baseline',
    href: location.href.replace(token, '<capability>'),
    origin: location.origin,
    isSecureContext: window.isSecureContext,
  };
  try {
    results.storage = await checkStorage();
    results.cryptoSubtle = await checkCrypto();
    const ping = await ping50(50);
    results.ping50 = { ...stats(ping.samples), failures: ping.failures, firstFailure: ping.firstFailure };
    results.pingWithExtraHeader = await pingWithExtraHeader();
    results.invokeGetState = await checkInvoke();
    results.assetRuns = await checkAsset();
    results.external = HTTPS ? await checkExternalBlock() : 'skipped (no interception in baseline)';
    await report({ phase: 'main', results });
    log(JSON.stringify(results, null, 1));
  } catch (error) {
    await report({ phase: 'error', error: String(error), stack: error?.stack, results }).catch(() => {});
  }
  await closeWindow();
}

run();
