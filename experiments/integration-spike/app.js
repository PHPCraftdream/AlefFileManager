// M0.3 integration spike page: measures rAF only. Pass/fail comes from the Rust verdict.
const token = new URLSearchParams(location.hash.slice(1)).get('capability') ?? '';
const auth = { Authorization: `Bearer ${token}` };
const logElement = document.getElementById('log');
const log = line => { logElement.textContent += `${line}\n`; };

let frames = 0;
let reports = 0;
const started = Date.now();
const count = () => { frames += 1; requestAnimationFrame(count); };
requestAnimationFrame(count);

const report = payload => fetch('native://spike/report', { method: 'POST', headers: auth, body: JSON.stringify(payload) });
const closeWindow = () => fetch('native://invoke/', {
  method: 'POST',
  headers: { ...auth, 'Content-Type': 'application/json' },
  body: JSON.stringify({ command: 'runtime.window', arguments: { action: 'close' } }),
});

const REPORTS = 26; // ~13 s of measurements, then the page closes the window

setInterval(async () => {
  reports += 1;
  const payload = { report: reports, tMs: Date.now() - started, raf: frames, note: 'm0-integration' };
  log(JSON.stringify(payload));
  try { await report(payload); } catch (error) { log(`report failed: ${error}`); }
  if (reports >= REPORTS) {
    const summary = { phase: 'measure', totalRaf: frames, reports };
    log(JSON.stringify(summary));
    try { await report(summary); } catch (error) { log(`summary report failed: ${error}`); }
    await new Promise(resolve => setTimeout(resolve, 500));
    try { await closeWindow(); } catch (error) { log(`close failed: ${error}`); }
  }
}, 500);
