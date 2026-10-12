// The store: the runner starts this application twice (`--step=write`, `--step=read`); the second run is
// a new process and finds what the first one kept.
import { api, guard, rejection, report, suite, verdict } from './harness.js';

const { store } = api;

const SAMPLE = {
  text: 'héllo — мир 🌍', number: -12.5, flag: true, nothing: null,
  list: [1, 'two', { three: 3 }], nested: { a: { b: { c: [] } } },
};

const same = (left, right) => JSON.stringify(left) === JSON.stringify(right);

async function expectCode(what, promise, code) {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`${what}: expected ${code}, got ${error?.code ?? 'success'}`);
}

async function write(check) {
  await report(`path appData ${await api.path.appData()}`);

  await check('store-values-of-every-json-kind-come-back', async () => {
    for (const [key, value] of Object.entries(SAMPLE)) {
      await store.set(key, value);
      const back = await store.get(key);
      if (!same(back, value)) throw new Error(`${key}: ${JSON.stringify(back)}`);
    }
    if (await store.get('never-written') !== undefined) throw new Error('a key that is not there is undefined');
    if (await store.get('nothing') !== null) throw new Error('a stored null is null');
    await expectCode('undefined', store.set('x', undefined), 'INVALID_ARGUMENT');
  });

  await check('store-keys-come-in-order-by-prefix-and-delete', async () => {
    for (const key of ['p/b', 'p/a', 'q']) await store.set(key, 1);
    if (!same(await store.keys('p/'), ['p/a', 'p/b'])) throw new Error(JSON.stringify(await store.keys('p/')));
    await store.delete('p/a');
    await store.delete('p/a');
    if (!same(await store.keys('p/'), ['p/b'])) throw new Error('after delete');
    await store.set('gone', 'soon');
    await store.delete('gone');
    if (await store.get('gone') !== undefined) throw new Error('deleted');
  });

  await check('store-areas-are-apart-and-named-with-care', async () => {
    const prefs = await store.open('prefs');
    await prefs.set('lang', 'ru');
    if (await store.get('lang') !== undefined) throw new Error('the default area sees an area');
    if (await prefs.get('lang') !== 'ru') throw new Error('the area does not keep');
    if (!same(await prefs.keys(), ['lang'])) throw new Error(JSON.stringify(await prefs.keys()));
    await expectCode('a name with a space', store.open('a b'), 'INVALID_ARGUMENT');
  });

  await check('store-a-value-is-up-to-what-a-call-carries', async () => {
    await store.set('big', 'x'.repeat(200 * 1024));
    if ((await store.get('big')).length !== 200 * 1024) throw new Error('the big value changed');
    await expectCode('300 KiB', store.set('too-big', 'x'.repeat(300 * 1024)), 'INVALID_ARGUMENT');
    if (await store.get('too-big') !== undefined) throw new Error('a refused value was kept');
  });

  await check('store-flush-waits-for-the-disk', async () => {
    await store.flush();
  });
}

async function read(check) {
  await report(`path appData ${await api.path.appData()}`);

  await check('store-what-the-first-run-kept-is-there-after-the-restart', async () => {
    for (const [key, value] of Object.entries(SAMPLE)) {
      const back = await store.get(key);
      if (!same(back, value)) throw new Error(`${key}: ${JSON.stringify(back)}`);
    }
    if (!same(await store.keys('p/'), ['p/b'])) throw new Error('keys');
    if (await store.get('gone') !== undefined) throw new Error('a deleted key came back');
    if ((await store.get('big')).length !== 200 * 1024) throw new Error('the big value');
    if (await store.get('too-big') !== undefined) throw new Error('a refused value came back');
    const prefs = await store.open('prefs');
    if (await prefs.get('lang') !== 'ru') throw new Error('the area');
  });

  await check('store-it-goes-on-working-after-the-restart', async () => {
    await store.set('after', { run: 2 });
    if (!same(await store.get('after'), { run: 2 })) throw new Error('written after the restart');
    await store.delete('after');
  });
}

async function main() {
  const { check, failed } = suite();
  const { step } = (await api.app.args()).parsed;
  if (step === 'write') await write(check);
  else if (step === 'read') await read(check);
  else throw new Error(`unknown step ${step}`);
  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
