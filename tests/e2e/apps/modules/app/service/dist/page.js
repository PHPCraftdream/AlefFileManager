// A service through the real runtime: no window and no console. It serves HTTP on a port of the runner, which asks
// it for `/ping`, then ends it: by `/quit` (the page quits with a code) or by a signal (the page is asked before it
// ends, as `app.quit` asks it, and does not stand in the way).
import { api, guard, rejection, report, suite, verdict } from './harness.js';

const { app, http } = api;

async function main() {
  const t = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  let asked = 0;

  await check('service-serves-without-a-window', async () => {
    const server = await http.serve({ port: t.port });
    (async () => {
      for await (const request of server) {
        const path = new URL(request.url, 'http://service').pathname;
        if (path === '/quit') {
          await request.respond({ body: 'bye' });
          await app.exit(5);
        }
        await request.respond({ body: path === '/ping' ? 'pong' : 'unknown' });
      }
    })().catch(error => console.error(`ALEF_E2E-page the service failed ${error?.message ?? error}`));
  });

  await check('service-is-asked-before-it-ends-on-a-signal', async () => {
    await app.on('before-quit', async () => {
      asked += 1;
      await report(`check service-heard-before-quit ok ${asked}`);
    });
  });

  await check('service-has-no-console-and-no-window', async () => {
    for (const [what, attempt] of [
      ['app.stdout', () => app.stdout.getWriter().write(new Uint8Array([65]))],
      ['window.all', () => api.window.all()],
    ]) {
      const error = await rejection(attempt());
      if (error?.code !== 'NOT_AVAILABLE') throw new Error(`${what}: ${error?.code ?? 'it succeeded'}`);
    }
  });

  await verdict(failed());
}

guard(main);
