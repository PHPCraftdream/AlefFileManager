// SPDX-License-Identifier: MIT OR Apache-2.0
import { api, guard, report, suite, until, verdict } from './harness.js';
const options = () => ({ signal: AbortSignal.timeout(10000) });
const expect = (ok, message) => { if (!ok) throw new Error(message); };
guard(async () => {
  const { check, failed } = suite();
  try {
    await check('autostart-enable', async () => {
      expect(await api.app.autostart.isEnabled(options()) === false, 'initially enabled');
      await api.app.autostart.enable(options());
      await api.app.autostart.enable(options());
      expect(await api.app.autostart.isEnabled(options()) === true, 'not enabled');
    });
    if (failed().length) throw new Error('enable failed');
    await report('integration native-ready');
    const release = await api.app.env('ALEF_E2E_RELEASE', options());
    await until(() => api.fs.exists(release, options()), 90000, 'runner independent native verification');
    await check('autostart-disable', async () => {
      await api.app.autostart.disable(options());
      await api.app.autostart.disable(options());
      expect(await api.app.autostart.isEnabled(options()) === false, 'still enabled');
    });
  } finally {
    await api.app.autostart.disable(options());
  }
  await verdict(failed());
  void api.app.quit(0, options());
});
