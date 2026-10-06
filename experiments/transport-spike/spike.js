// Transport spike, see docs/TRANSPORT.md. Results go to the runtime stderr.
const token = new URLSearchParams(location.hash.slice(1)).get('capability') ?? '';
const auth = { Authorization: `Bearer ${token}` };
const logElement = document.getElementById('log');
const results = {};

const log = line => { logElement.textContent += `${line}\n`; };
const nowMs = () => Date.now();
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const stats = values => {
  const sorted = [...values].sort((a, b) => a - b);
  const at = q => sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))];
  return { n: sorted.length, min: sorted[0], p50: at(0.5), p90: at(0.9), max: sorted.at(-1) };
};
const round = value => Math.round(value * 100) / 100;

async function report(payload) {
  await fetch('native://spike/report', { method: 'POST', headers: auth, body: JSON.stringify(payload) });
}

// Reads a stream of fixed-size chunks; records per-chunk latency and read count.
async function readStream({ chunks, size, delay, label, signal, readDelay = 0 }) {
  const started = nowMs();
  const response = await fetch(
    `native://spike/stream?chunks=${chunks}&size=${size}&delay_ms=${delay}&label=${label}`,
    { headers: auth, signal },
  );
  const headersAt = nowMs() - started;
  if (readDelay) await sleep(readDelay);
  const reader = response.body.getReader();
  let pending = new Uint8Array(0);
  let reads = 0;
  let received = 0;
  const latencies = [];
  const arrivals = [];
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    reads += 1;
    const merged = new Uint8Array(pending.length + value.length);
    merged.set(pending);
    merged.set(value, pending.length);
    pending = merged;
    while (pending.length >= size) {
      const view = new DataView(pending.buffer, pending.byteOffset, 12);
      const now = nowMs();
      latencies.push(now - view.getFloat64(0, true));
      arrivals.push(now - started);
      received += 1;
      pending = pending.slice(size);
    }
  }
  const total = nowMs() - started;
  return {
    label, chunks, size, delay, received, reads,
    headersAtMs: round(headersAt),
    firstChunkAtMs: round(arrivals[0] ?? -1),
    totalMs: round(total),
    latencyMs: Object.fromEntries(Object.entries(stats(latencies)).map(([k, v]) => [k, round(v)])),
    throughputMBps: round((chunks * size) / 1048576 / (total / 1000)),
  };
}

async function echo(bytes) {
  const payload = new Uint8Array(bytes);
  for (let i = 0; i < payload.length; i += 4096) payload[i] = i & 255;
  const started = nowMs();
  const response = await fetch('native://spike/echo', {
    method: 'POST', headers: { ...auth, 'Content-Type': 'application/octet-stream' }, body: payload,
  });
  const result = new Uint8Array(await response.arrayBuffer());
  const total = nowMs() - started;
  let intact = result.length === payload.length;
  for (let i = 0; intact && i < payload.length; i += 4096) intact = result[i] === payload[i];
  return { mib: bytes / 1048576, status: response.status, intact, totalMs: round(total),
    roundTripMBps: round((2 * bytes) / 1048576 / (total / 1000)) };
}

async function run() {
  const storage = {};
  for (const name of ['localStorage', 'sessionStorage', 'indexedDB']) {
    try { storage[name] = window[name] ? 'available' : 'missing'; window[name]?.length; } catch (error) { storage[name] = String(error); }
  }
  storage.origin = location.origin;
  if (new URLSearchParams(location.hash.slice(1)).get('phase') === 'after-reload') {
    await sleep(3000);
    await report({ phase: 'reload', note: 'page reloaded during long stream; check runtime log for receiver closed' });
    await fetch('native://invoke/', {
      method: 'POST', headers: { ...auth, 'Content-Type': 'application/json' },
      body: JSON.stringify({ command: 'runtime.window', arguments: { action: 'close' } }),
    });
    return;
  }
  try {
    log('1. incremental delivery');
    results.incremental = await readStream({ chunks: 20, size: 1024, delay: 100, label: 'incremental' });
    log(JSON.stringify(results.incremental));

    log('2. stream throughput');
    results.throughput = await readStream({ chunks: 256, size: 262144, delay: 0, label: 'throughput' });
    log(JSON.stringify(results.throughput));

    log('3. buffering without reader (reader starts after 2 s)');
    results.buffering = await readStream({ chunks: 64, size: 1048576, delay: 0, label: 'buffering', readDelay: 2000 });
    log(JSON.stringify(results.buffering));

    log('4. abort after 1 s');
    const controller = new AbortController();
    setTimeout(() => controller.abort(), 1000);
    try {
      await readStream({ chunks: 200, size: 1024, delay: 50, label: 'abort', signal: controller.signal });
      results.abort = { aborted: false };
    } catch (error) {
      results.abort = { aborted: true, error: String(error) };
    }
    await sleep(10000);
    log(JSON.stringify(results.abort));

    log('5a. upload only / download only');
    results.upload = [];
    results.download = [];
    for (const mib of [1, 16]) {
      const payload = new Uint8Array(mib * 1048576);
      let started = nowMs();
      const sink = await fetch('native://spike/sink', {
        method: 'POST', headers: { ...auth, 'Content-Type': 'application/octet-stream' }, body: payload,
      });
      const length = Number(await sink.text());
      let total = nowMs() - started;
      results.upload.push({ mib, intact: length === payload.length, totalMs: total, MBps: round(mib / (total / 1000)) });
      started = nowMs();
      const source = await fetch(`native://spike/source?size=${mib * 1048576}`, { headers: auth });
      const bytes = (await source.arrayBuffer()).byteLength;
      total = nowMs() - started;
      results.download.push({ mib, intact: bytes === payload.length, totalMs: total, MBps: round(mib / (total / 1000)) });
    }
    log(JSON.stringify({ upload: results.upload, download: results.download }));

    log('5. binary echo');
    results.echo = [];
    for (const mib of [1, 16]) results.echo.push(await echo(mib * 1048576));
    log(JSON.stringify(results.echo));

    await report({ phase: 'main', storage, results });
  } catch (error) {
    await report({ phase: 'main', error: String(error), stack: error?.stack, results });
  }
  log('6. reload during long stream');
  location.hash = `${location.hash.slice(1)}&phase=after-reload`;
  readStream({ chunks: 200, size: 1024, delay: 50, label: 'reload' }).catch(() => {});
  await sleep(1000);
  location.reload();
}

run();
