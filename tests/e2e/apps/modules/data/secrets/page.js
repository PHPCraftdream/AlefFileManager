// The module secrets through the real transport. An end-to-end run keeps the secrets in memory (the
// credential store of the machine is never touched): in `allowed` mode the user allowed the right and the
// page works with it, in `substituted` mode he chose a stand-in, in `denied` mode he said no.
import { api, guard, rejection, suite, verdict } from './harness.js';

const { secrets } = api;

const expectCode = async (what, promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`${what}: ${error?.code ?? 'it succeeded'}, expected ${code}`);
  return error;
};

const same = (left, right) => left !== null && left.length === right.length && left.every((byte, index) => byte === right[index]);

async function allowed(check) {
  await check('secrets-a-secret-comes-back-as-it-went', async () => {
    if (await secrets.get('mail', 'me') !== null) throw new Error('a secret that was never kept is null');
    const every = Uint8Array.from({ length: 256 }, (_, index) => index);
    await secrets.set('mail', 'me', every);
    if (!same(await secrets.get('mail', 'me'), every)) throw new Error('the bytes changed');
    const longest = new Uint8Array(1024).fill(0xa5);
    await secrets.set('mail', 'me', longest);
    if (!same(await secrets.get('mail', 'me'), longest)) throw new Error('a second set did not replace the first');
    await secrets.set('chat', 'сервис — 🙂', 'пароль 🌍');
    if (await secrets.getText('chat', 'сервис — 🙂') !== 'пароль 🌍') throw new Error('text did not come back');
    if (await secrets.get('chat', 'me') !== null) throw new Error('another account saw it');
    if (!await secrets.delete('mail', 'me')) throw new Error('delete of a kept secret said there was none');
    if (await secrets.delete('mail', 'me')) throw new Error('delete of nothing said there was something');
    if (await secrets.get('mail', 'me') !== null) throw new Error('deleted, and still there');
    if (await secrets.getText('chat', 'сервис — 🙂') !== 'пароль 🌍') throw new Error('a delete took another secret');
  });

  await check('secrets-the-limits-and-the-names-are-told', async () => {
    await expectCode('an empty secret', secrets.set('mail', 'me', ''), 'INVALID_ARGUMENT');
    await expectCode('1025 bytes', secrets.set('mail', 'me', new Uint8Array(1025)), 'INVALID_ARGUMENT');
    await expectCode('an empty service', secrets.get('', 'me'), 'INVALID_ARGUMENT');
    await expectCode('a long account', secrets.get('mail', 'x'.repeat(129)), 'INVALID_ARGUMENT');
    await expectCode('a line break', secrets.delete('ma\nil', 'me'), 'INVALID_ARGUMENT');
    if (await secrets.get('mail', 'me') !== null) throw new Error('a refused secret was kept');
    await secrets.set('mail', 'me', new Uint8Array(1));
    await secrets.delete('mail', 'me');
  });
}

async function substituted(check) {
  await check('secrets-a-stand-in-keeps-what-is-written-until-the-run-ends', async () => {
    if (await secrets.get('mail', 'me') !== null) throw new Error('the stand-in is not empty');
    await secrets.set('mail', 'me', 'made up');
    if (await secrets.getText('mail', 'me') !== 'made up') throw new Error('the stand-in does not keep');
    if (!await secrets.delete('mail', 'me')) throw new Error('delete');
    if (await secrets.get('mail', 'me') !== null) throw new Error('deleted, and still there');
  });
}

async function denied(check) {
  await check('secrets-a-denied-right-keeps-every-command-out', async () => {
    const calls = {
      get: () => secrets.get('mail', 'me'),
      getText: () => secrets.getText('mail', 'me'),
      set: () => secrets.set('mail', 'me', 'x'),
      delete: () => secrets.delete('mail', 'me'),
    };
    for (const [name, call] of Object.entries(calls)) {
      const error = await expectCode(name, call(), 'PERMISSION_DENIED');
      if (error.details?.permission !== 'secrets') throw new Error(`${name}: ${JSON.stringify(error.details)}`);
    }
  });
}

async function main() {
  const t = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  if (t.mode === 'substituted') await substituted(check);
  else if (t.mode === 'denied') await denied(check);
  else await allowed(check);
  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
