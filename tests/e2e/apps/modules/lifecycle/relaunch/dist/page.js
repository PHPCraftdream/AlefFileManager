// Relaunch scenario: the first instance asks for a restart, the second instance (started by the
// runtime with the same arguments and environment, sharing the log pipe) reports the verdict and quits.
// `e2e.once` creates the marker file named by ALEF_E2E_MARKER and tells which instance is first.
import { api, guard, report, suite, verdict } from './harness.js';

async function main() {
  const marker = await api.app.env('ALEF_E2E_MARKER');
  if (!marker) throw new Error('the runner did not pass ALEF_E2E_MARKER');
  const first = await api.call('e2e.once', { path: marker });
  const args = await api.app.args();
  if (first) {
    await report(`relaunch first-instance raw=${JSON.stringify(args.raw)}`);
    await api.app.relaunch();
    return;
  }
  const { check, failed } = suite();
  await check('relaunch-starts-a-second-instance-with-the-same-arguments', async () => {
    if (JSON.stringify(args.raw) !== '["--label=relaunch"]' || args.parsed.label !== 'relaunch') {
      throw new Error(`arguments of the second instance: ${JSON.stringify(args)}`);
    }
  });
  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
