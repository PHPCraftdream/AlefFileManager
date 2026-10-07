// M0.5 spike page: what an application can do in a hidden WebView with no window. Every line it
// reports reaches stdout as `ALEF_E2E HEADLESS ...`; the runner (../run.mjs) measures around it.
import { api, report, sleep } from './harness.js';

const begun = performance.now();
const since = () => Math.round(performance.now() - begun);

async function main() {
  const { parsed } = await api.app.args();
  const hold = Number(parsed['hold-ms'] ?? 0);
  const info = await api.app.info();
  await report(`HEADLESS app.info ${JSON.stringify(info)} page-ms=${since()}`);
  const echoed = await api.call('e2e.echo', { hello: 'headless' });
  await report(`HEADLESS echo ${JSON.stringify(echoed)}`);

  // Timers and the event loop of the page run.
  let ticks = 0;
  const timer = setInterval(() => { ticks += 1; }, 100);
  // A hidden WebView should not be painted: frames of requestAnimationFrame say whether it is.
  let frames = 0;
  const loop = () => { frames += 1; requestAnimationFrame(loop); };
  if (parsed.raf) requestAnimationFrame(loop);

  // fetch reaches the transport (the same one the pages with a window use).
  const response = await fetch('native://call/e2e.echo', { method: 'GET' }).then(
    answer => `status=${answer.status}`,
    error => `error=${error?.message ?? error}`,
  );
  await report(`HEADLESS fetch ${response}`);

  await sleep(Math.max(hold, 1000));
  clearInterval(timer);
  await report(`HEADLESS timers ticks=${ticks} raf=${Boolean(parsed.raf)} raf-frames=${frames} held-ms=${since()}`);

  // A window command has nobody to show it to.
  const refused = await api.window.current().then(win => win.state()).then(() => 'answered', error => error?.code ?? String(error));
  await report(`HEADLESS window.state ${refused}`);

  await report('HEADLESS page-done');
  await api.app.quit(0);
}

main().catch(async error => {
  await report(`HEADLESS page-failed ${error?.message ?? error}`);
  await api.app.quit(3).catch(() => {});
});
