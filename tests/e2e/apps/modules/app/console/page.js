// A console utility through the real runtime: no window, the input comes on stdin, the answer goes out on stdout
// (every byte changed, so that the runner can tell the answer from the input), a line goes to stderr, and the
// exit code is the one the page asks for. Started by the runner with the input on the pipe of stdin.
import { api, guard, rejection, report, suite, verdict } from './harness.js';

const { app } = api;

/** Every byte of the input as the runner works it out again. */
const change = byte => byte ^ 0x5a;

async function main() {
  const t = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  let length = 0;

  await check('console-stdin-comes-whole-and-goes-out-changed-on-stdout', async () => {
    const reader = app.stdin.getReader();
    const writer = app.stdout.getWriter();
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.length;
      await writer.write(value.map(change));
    }
    await writer.close();
    if (length !== t.size) throw new Error(`${length} bytes came on stdin, expected ${t.size}`);
  });

  await check('console-stderr-carries-a-line-of-its-own', async () => {
    const writer = app.stderr.getWriter();
    await writer.write(new TextEncoder().encode('ALEF_CONSOLE a line for stderr\n'));
    await writer.close();
  });

  await check('console-the-windows-and-the-dialogs-are-not-available', async () => {
    for (const [what, attempt] of [['window.all', () => api.window.all()], ['dialog.message', () => api.dialog.message({ message: 'x' })]]) {
      const error = await rejection(attempt());
      if (error?.code !== 'NOT_AVAILABLE') throw new Error(`${what}: ${error?.code ?? 'it succeeded'}`);
    }
  });

  await check('console-stdin-is-taken-once', async () => {
    const again = await rejection(api.call('app.stdin', null));
    if (again?.code !== 'BUSY') throw new Error(`a second stdin: ${again?.code ?? 'it succeeded'}`);
  });

  await report(`input ${length}`);
  await verdict(failed());
  await app.exit(7);
}

guard(main);
