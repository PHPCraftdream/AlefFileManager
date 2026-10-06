// M0.2 multiwindow spike page (docs/stages/m0-spikes.md). Passive during the scenario;
// the runtime drives resizes and calls __alefMwSummary at the end so the machine-readable
// verdict reaches runtime stderr through the report route. Report failures are logged to
// the console, which the runtime delegates also print to stderr.
const token = new URLSearchParams(location.hash.slice(1)).get('capability') ?? '';
const auth = { Authorization: `Bearer ${token}` };
const id = Math.random().toString(36).slice(2, 8);
const logElement = document.getElementById('log');
const log = line => { logElement.textContent += `${line}\n`; };
const report = payload =>
  fetch('native://spike/report', { method: 'POST', headers: auth, body: JSON.stringify(payload) })
    .then(response => { if (!response.ok) console.error(`mw report failed: HTTP ${response.status}`); })
    .catch(error => console.error(`mw report failed: ${String(error)}`));

window.addEventListener('load', () => {
  log(`window ${id} loaded`);
  report({ kind: 'mw-loaded', id, w: innerWidth, h: innerHeight, dpr: devicePixelRatio });
});
window.addEventListener('resize', () => log(`resize ${innerWidth}x${innerHeight}`));
window.addEventListener('__alef_runtime_event__', event => log(JSON.stringify(event.detail)));
// Final verdict from the runtime; the POST prints `spike report: {"kind":"mw-summary",...}`.
window.__alefMwSummary = checks => {
  report({ kind: 'mw-summary', checks });
};
