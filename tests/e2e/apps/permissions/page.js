// Permissions scenario: the manifest's fs.read scope and app.env list decide, in the runtime, which
// calls reach a handler (docs/stages/m1-core.md: "no permission -> PERMISSION_DENIED; scope outside
// the manifest -> refusal"). `e2e.fsRead/fsWrite/appEnv` only reply when the dispatcher allowed them.
import { AlefError } from './src/index.js';
import { api, guard, rejection, suite, verdict } from './harness.js';

async function main() {
  const { app, sep } = await (await fetch('targets.json')).json();
  const inside = (...parts) => [app, 'data', ...parts].join(sep);
  const { check, failed } = suite();

  const denied = async (command, target) => {
    const error = await rejection(api.call(command, { target }));
    if (!(error instanceof AlefError) || error.code !== 'PERMISSION_DENIED' || error.status !== 403) {
      throw new Error(`${command} ${JSON.stringify(target)} was not denied: ${error ?? 'it succeeded'}`);
    }
    if (error.message.includes(app)) throw new Error('the denial leaks the application path');
  };

  await check('fs-read-inside-the-scope-is-allowed', async () => {
    for (const target of [inside('note.txt'), inside('not-created-yet.txt'), inside('sub', 'deeper', 'x.txt')]) {
      const reply = await api.call('e2e.fsRead', { target });
      if (reply.allowed !== true) throw new Error(`${target} was not allowed`);
    }
  });
  await check('fs-read-outside-the-scope-is-denied', async () => {
    const outside = [
      [app, 'secret.txt'].join(sep),
      [app, 'data', '..', 'secret.txt'].join(sep),
      [app, 'other', 'x.txt'].join(sep),
      [app, 'data-lookalike', 'x.txt'].join(sep),
      `data${sep}note.txt`,
      '',
      `${inside('note.txt')}\u0000`,
    ];
    for (const target of outside) await denied('e2e.fsRead', target);
  });
  await check('fs-write-without-the-permission-is-denied', async () => {
    await denied('e2e.fsWrite', inside('note.txt'));
    await denied('e2e.fsWrite', inside('new.txt'));
  });
  await check('app-env-only-the-listed-variable-is-allowed', async () => {
    const reply = await api.call('e2e.appEnv', { target: 'ALEF_E2E_ALLOWED' });
    if (reply.allowed !== true) throw new Error('the listed variable was refused');
    await denied('e2e.appEnv', 'PATH');
    await denied('e2e.appEnv', 'alef_e2e_allowed ');
  });
  await check('a-denied-call-does-not-poison-the-session', async () => {
    await denied('e2e.fsRead', [app, 'secret.txt'].join(sep));
    const reply = await api.call('e2e.fsRead', { target: inside('note.txt') });
    if (reply.allowed !== true) throw new Error('an allowed call failed after a denial');
  });
  await verdict(failed());
}

guard(main);
