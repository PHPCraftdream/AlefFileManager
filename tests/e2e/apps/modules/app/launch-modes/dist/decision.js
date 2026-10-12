// SPDX-License-Identifier: MIT OR Apache-2.0
import { api, guard, rejection, report, suite, verdict } from './harness.js';

const { app, http } = api;

async function main() {
  const args = await app.args();
  const mode = args.parsed.mode;
  // Deliberately leave the launch decision to the runtime's bounded watchdog.
  if (mode === 'undecided') return;
  const { check, failed } = suite();
  await check('launch-starts-undecided', async () => {
    if (await app.mode() !== 'undecided') throw new Error('entry already selected a mode');
    for (const command of ['app.stdin', 'app.stdout', 'app.stderr']) {
      const error = await rejection(api.call(command, null));
      if (error?.code !== 'NOT_AVAILABLE') throw new Error(`${command}: ${error?.code ?? 'succeeded'}`);
    }
  });
  await report(`launch-decision mode=${mode} raw=${JSON.stringify(args.raw)}`);
  await app.launch(mode);
  // Window selection replaces this document with a separate process and child.html.
  if (mode === 'window') return;
  await check('launch-same-mode-is-a-no-op', async () => {
    await app.launch(mode);
    if (await app.mode() !== mode) throw new Error('repeat changed the selected mode');
    const error = await rejection(app.launch(mode === 'cli' ? 'foreground' : 'cli'));
    if (error?.code !== 'ALREADY_EXISTS') throw new Error(`different mode: ${error?.code ?? 'succeeded'}`);
    const invalid = await rejection(api.call('app.launch', { mode: 'bogus' }));
    if (invalid?.code !== 'INVALID_ARGUMENT') throw new Error(`invalid mode: ${invalid?.code ?? 'succeeded'}`);
  });
  await check('launch-keeps-arguments', async () => {
    if (JSON.stringify(await app.args()) !== JSON.stringify(args)) throw new Error('arguments changed');
  });
  if (mode === 'cli') {
    const writer = app.stdout.getWriter();
    await writer.write(new TextEncoder().encode('ALEF_LAUNCH_CLI selected cli\n'));
    await writer.close();
    await verdict(failed());
    await app.exit(7);
  } else if (mode === 'foreground') {
    await app.on('before-quit', async () => report('launch-before-quit'));
    const server = await http.serve({ port: args.parsed.port });
    await verdict(failed());
    await report('launch-foreground-ready');
    for await (const request of server) {
      const path = new URL(request.url, 'http://service').pathname;
      await request.respond({ body: path === '/ping' ? 'pong' : path === '/quit' ? 'bye' : 'unknown' });
      if (path === '/quit') await app.exit(5);
    }
  } else {
    throw new Error(`unexpected mode ${mode}`);
  }
}

guard(main);
