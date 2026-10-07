// Consent scenarios: the user decided before the start (the runner's script of answers, or `alef
// permissions`), and the application finds out nothing it should not. Allowed rights are the real
// thing, a stand-in is indistinguishable from the real thing but touches nothing, a denial is the error
// of an unlisted right, and what has no stand-in is refused when the user chose one. In `narrow` mode
// the runner changes the decisions while this page runs.
import { api, guard, report, rejection, sleep, suite, verdict } from './harness.js';

const ALLOWED = 'https://allowed.example/page';
const STAND_IN = 'https://stand-in.example/page';

const shape = error => ({ code: error?.code, message: error?.message, details: error?.details, status: error?.status });

async function decided(check, t) {
  await check('consent-an-environment-variable-is-real-a-stand-in-or-denied', async () => {
    const value = await api.app.env('ALEF_E2E_ENV_ALLOWED');
    if (value !== 'yes') throw new Error(`allowed: ${JSON.stringify(value)}`);
    const unset = await api.app.env('ALEF_E2E_ENV_UNSET');
    if (unset !== undefined) throw new Error(`unset: ${JSON.stringify(unset)}`);
    const stood = await api.app.env('ALEF_E2E_ENV_SUBSTITUTED');
    if (stood !== unset) throw new Error(`a stand-in must answer like a variable that is not set, not ${JSON.stringify(stood)}`);
    const denied = await rejection(api.app.env('ALEF_E2E_ENV_DENIED'));
    if (denied?.code !== 'PERMISSION_DENIED') throw new Error(`denied: ${denied?.code ?? 'it succeeded'}`);
  });

  await check('consent-a-denial-is-the-error-of-a-right-the-manifest-never-listed', async () => {
    const byUser = shape(await rejection(api.app.env('ALEF_E2E_ENV_DENIED')));
    const byManifest = shape(await rejection(api.app.env('ALEF_E2E_ENV_NEVER_LISTED')));
    if (JSON.stringify(byUser) !== JSON.stringify(byManifest)) {
      throw new Error(`the application can tell them apart: ${JSON.stringify(byUser)} / ${JSON.stringify(byManifest)}`);
    }
  });

  await check('consent-the-list-of-variables-has-only-what-is-really-given', async () => {
    const all = await api.app.env();
    if (JSON.stringify(all) !== JSON.stringify({ ALEF_E2E_ENV_ALLOWED: 'yes' })) throw new Error(`env ${JSON.stringify(all)}`);
  });

  await check('consent-a-substituted-address-reports-success-and-opens-nothing', async () => {
    await api.shell.openExternal(STAND_IN);
    await api.shell.openExternal(ALLOWED);
  });

  await check('consent-a-command-without-a-stand-in-refuses-the-substitute', async () => {
    const error = await rejection(api.window.create({ label: 'extra', url: '/index.html', width: 100, height: 100 }));
    if (error?.code !== 'PERMISSION_DENIED') throw new Error(`window.create: ${error?.code ?? 'it succeeded'}`);
  });

  await check('consent-the-folder-of-the-runtime-is-out-of-reach-of-every-right', async () => {
    await api.shell.openPath(t.note);
    for (const path of [t.home, t.decisions]) {
      const error = await rejection(api.shell.openPath(path));
      if (error?.code !== 'PERMISSION_DENIED') throw new Error(`${path}: ${error?.code ?? 'it was opened'}`);
    }
  });

  await check('consent-the-commands-of-the-permission-window-are-not-the-applications', async () => {
    for (const command of ['runtime.consent.request', 'runtime.consent.answer', 'runtime.consent.cancel']) {
      const error = await rejection(api.call(command, command === 'runtime.consent.answer' ? { decisions: [] } : null));
      if (error?.code !== 'NOT_FOUND') throw new Error(`${command}: ${error?.code ?? 'it succeeded'}`);
    }
  });
}

async function narrowed(check) {
  await check('consent-a-right-taken-back-applies-at-once-and-a-right-given-waits-for-the-next-start', async () => {
    if (await api.app.env('ALEF_E2E_ENV_ALLOWED') !== 'yes') throw new Error('the variable was not given at the start');
    const closed = await rejection(api.app.env('ALEF_E2E_ENV_DENIED'));
    if (closed?.code !== 'PERMISSION_DENIED') throw new Error('the other variable was not denied at the start');
    // The runner now gives one more right and takes one back, in this order.
    await report('narrowing-ready');
    const deadline = performance.now() + 20000;
    for (;;) {
      const error = await rejection(api.app.env('ALEF_E2E_ENV_ALLOWED'));
      if (error?.code === 'PERMISSION_DENIED') break;
      if (performance.now() > deadline) throw new Error('the right taken back still works after 20 s');
      await sleep(100);
    }
    // The store says that the other right is given too, and the store was read since: it is not in force.
    const still = await rejection(api.app.env('ALEF_E2E_ENV_DENIED'));
    if (still?.code !== 'PERMISSION_DENIED') throw new Error('a right given while the application runs took effect');
  });
}

async function main() {
  const t = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  if (t.mode === 'narrow') await narrowed(check);
  else await decided(check, t);
  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
