// SPDX-License-Identifier: MIT OR Apache-2.0
import { api, guard, rejection, report, sleep, suite, until, verdict } from './harness.js';
const options = () => ({ signal: AbortSignal.timeout(10000) });
const equal = (actual, expected) => {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${JSON.stringify(actual)} != ${JSON.stringify(expected)}`);
};
guard(async () => {
  const targetResponse = await fetch('targets.json', { signal: AbortSignal.timeout(10000) });
  const { platform, startupUrl, secondUrl, cwd } = await targetResponse.json();
  const args = await api.app.args(options());
  const { check, failed } = suite();
  if (args.parsed.role === 'second') {
    await check('later-instance-false', async () => {
      equal(await api.app.requestSingleInstance(options()), false);
      equal(await api.app.requestSingleInstance(options()), false);
      equal(args.positional, ['other.txt']);
      equal(args.raw, ['--role=second', 'other.txt']);
    });
    await verdict(failed());
    void api.app.quit(0, options());
    return;
  }
  const urls = [];
  const instances = [];
  // A generic subscription must not drain launch URLs before app.on(open-url) is ready.
  const unrelated = await api.app.on('second-instance', info => instances.push(info), { signal: AbortSignal.timeout(115000) });
  await sleep(250);
  const unlisten = await api.app.on('open-url', info => urls.push(info.url), { signal: AbortSignal.timeout(115000) });
  try {
    await check('startup-queued-after-unrelated-subscription', async () => {
      await until(() => urls.length > 0, 10000, 'retained launch URL');
      equal(urls, [startupUrl]);
      equal(args.raw, ['--role=first', 'first.txt']);
      equal(args.positional, ['first.txt']);
    });
    await check('registration', async () => {
      if (platform === 'darwin') {
        equal((await rejection(api.app.registerDeepLinks(options())))?.code, 'NOT_AVAILABLE');
        await report('integration macOS registration NOT_AVAILABLE');
      } else {
        await api.app.registerDeepLinks(options());
        await api.app.registerDeepLinks(options());
      }
    });
    if (failed().length) throw new Error('startup/registration failed');
    await report('integration native-ready');
    const release = await api.app.env('ALEF_E2E_RELEASE', options());
    await until(() => api.fs.exists(release, options()), 90000, 'runner native verification');
    await check('first-instance-true', async () => {
      equal(await api.app.requestSingleInstance(options()), true);
      equal(await api.app.requestSingleInstance(options()), true);
    });
    if (failed().length) throw new Error('endpoint failed');
    await report('integration first-ready');
    await check('second-url-and-args-separated-no-duplicates', async () => {
      await until(() => urls.length >= 2 && instances.length >= 1, 90000, 'second instance URL and arguments');
      equal(urls, [startupUrl, secondUrl]);
      equal(instances[0].args, { raw: ['--role=second', 'other.txt'], parsed: { role: 'second' }, positional: ['other.txt'] });
      equal(instances[0].cwd, cwd);
      await sleep(750);
      equal(urls, [startupUrl, secondUrl]);
      equal(instances.length, 1);
    });
    await check('unregistration', async () => {
      if (platform === 'darwin') {
        equal((await rejection(api.app.unregisterDeepLinks(options())))?.code, 'NOT_AVAILABLE');
      } else {
        await api.app.unregisterDeepLinks(options());
        await api.app.unregisterDeepLinks(options());
      }
    });
  } finally {
    unlisten();
    unrelated();
    if (platform !== 'darwin') await api.app.unregisterDeepLinks(options());
  }
  await verdict(failed());
  void api.app.quit(0, options());
});
