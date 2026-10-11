// SPDX-License-Identifier: MIT OR Apache-2.0
import { api, guard, rejection, report, suite, verdict } from './harness.js';

async function main() {
  const { app, window } = api;
  const { check, failed } = suite();
  await check('launch-child-is-window', async () => {
    if (await app.mode() !== 'window') throw new Error('child is not in window mode');
    await app.launch('window');
    if (await app.mode() !== 'window') throw new Error('same-mode launch changed the child');
    const error = await rejection(app.launch('cli'));
    if (error?.code !== 'ALREADY_EXISTS') throw new Error(`changing mode: ${error?.code ?? 'succeeded'}`);
  });
  await check('launch-child-has-declared-window', async () => {
    const windows = await window.all();
    if (windows.length !== 1 || windows[0].label !== 'main') throw new Error(`windows: ${JSON.stringify(windows)}`);
  });
  await check('launch-child-has-no-stdio', async () => {
    for (const command of ['app.stdin', 'app.stdout', 'app.stderr']) {
      const error = await rejection(api.call(command, null));
      if (error?.code !== 'NOT_AVAILABLE') throw new Error(`${command}: ${error?.code ?? 'succeeded'}`);
    }
  });
  await check('launch-child-keeps-arguments', async () => {
    const args = await app.args();
    if (args.parsed.mode !== 'window' || JSON.stringify(args.raw) !== '["--mode","window"]'
      || args.positional.length !== 0 || Object.keys(args.parsed).length !== 1) {
      throw new Error(`args: ${JSON.stringify(args)}`);
    }
    await report(`launch-child raw=${JSON.stringify(args.raw)}`);
  });
  await verdict(failed());
  await app.quit(9);
}

guard(main);
