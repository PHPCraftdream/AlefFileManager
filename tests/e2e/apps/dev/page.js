// Development-server scenario: `alef --app <dir> --dev-url <url>` loads this page from an HTTP server
// on 127.0.0.1 instead of the files; the transport must still work from that origin.
import { api, guard, pattern, same, suite, until, verdict } from './harness.js';

async function main() {
  const { check, failed } = suite();

  await check('the-page-comes-from-the-development-server', async () => {
    if (location.protocol !== 'http:' || location.hostname !== '127.0.0.1') throw new Error(`origin ${location.origin}`);
    return location.origin;
  });
  await check('lib-connect-and-call-from-the-http-origin', async () => {
    const info = await api.connect();
    if (info.protocol !== 1) throw new Error(JSON.stringify(info));
    const value = await api.call('e2e.echo', { from: 'dev', n: [1, 2] });
    if (JSON.stringify(value) !== '{"from":"dev","n":[1,2]}') throw new Error('mismatch');
  });
  await check('binary-roundtrip-from-the-http-origin', async () => {
    const data = pattern(2 * 1024 * 1024);
    const back = await api.call('e2e.echo', {}, { body: data });
    if (!same(back, data)) throw new Error('bytes differ');
  });
  await check('events-reach-the-http-origin', async () => {
    const seen = [];
    const off = await api.on('runtime.window.state', payload => seen.push(payload));
    await api.nativeWindow.maximize();
    await until(() => seen.length > 0, 8000, 'a window state event');
    await api.nativeWindow.restore();
    off();
  });
  await check('a-foreign-http-origin-holding-the-capability-is-refused', async () => {
    const { probeOrigin } = await (await fetch('targets.json')).json();
    const capability = new URLSearchParams(location.hash.slice(1)).get('capability');
    const answer = new Promise((resolve, reject) => {
      window.addEventListener('message', event => {
        if (event.origin === probeOrigin) resolve(event.data);
      });
      setTimeout(() => reject(new Error('the probe never answered')), 8000);
    });
    const frame = document.createElement('iframe');
    frame.src = `${probeOrigin}/probe.html#capability=${capability}`;
    document.body.append(frame);
    const result = await answer;
    frame.remove();
    if (result.status === 200) throw new Error('another origin obtained a session with the bootstrap capability');
    return `answer ${result.status}`;
  });
  await verdict(failed());
}

guard(main);
