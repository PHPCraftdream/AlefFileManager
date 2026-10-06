// App module scenario: identity, the command line parsed by the manifest schema, the environment
// behind `permissions.app.env`, the working directory and `quit` with an exit code. Started as
// `alef --app <site> -- -p 8080 --verbose --label=<mode> y.txt`; mode `quit` ends with `app.quit(7)`.
import { AlefError } from './src/index.js';
import { api, guard, rejection, suite, verdict } from './harness.js';

// Structural equality, independent of the order of object keys.
const canonical = value => JSON.stringify(value, (_key, item) => (
  item && typeof item === 'object' && !Array.isArray(item)
    ? Object.fromEntries(Object.entries(item).sort(([a], [b]) => a.localeCompare(b)))
    : item));
const equal = (left, right) => canonical(left) === canonical(right);

async function main() {
  const { cwd } = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  const args = await api.app.args();

  await check('app-info-matches-the-manifest', async () => {
    const info = await api.app.info();
    const expected = { id: 'org.alef.e2e.modules.app', name: 'Alef e2e app', version: '4.5.6' };
    for (const [key, value] of Object.entries(expected)) {
      if (info[key] !== value) throw new Error(`${key}: ${JSON.stringify(info[key])}`);
    }
    if (!/^\d+\.\d+\.\d+/.test(info.runtimeVersion)) throw new Error(`runtimeVersion ${info.runtimeVersion}`);
    if (Object.keys(info).length !== 4) throw new Error(`unexpected fields ${Object.keys(info)}`);
  });
  await check('app-args-are-parsed-by-the-manifest-schema', async () => {
    const expected = ['-p', '8080', '--verbose', `--label=${args.parsed.label}`, 'y.txt'];
    if (!equal(args.raw, expected)) throw new Error(`raw ${JSON.stringify(args.raw)}`);
    if (!equal(args.parsed, { port: 8080, verbose: true, label: args.parsed.label })) throw new Error(`parsed ${JSON.stringify(args.parsed)}`);
    if (typeof args.parsed.port !== 'number') throw new Error('a number option is a number');
    if (!equal(args.positional, ['y.txt'])) throw new Error(`positional ${JSON.stringify(args.positional)}`);
  });
  await check('app-env-listed-variable-is-readable', async () => {
    const value = await api.app.env('ALEF_E2E_ENV_ALLOWED');
    if (value !== 'yes') throw new Error(`value ${JSON.stringify(value)}`);
    const unset = await api.app.env('ALEF_E2E_ENV_UNSET');
    if (unset !== undefined) throw new Error(`a listed variable that is not set is undefined, got ${JSON.stringify(unset)}`);
  });
  await check('app-env-unlisted-variable-is-denied', async () => {
    const error = await rejection(api.app.env('ALEF_E2E_ENV_SECRET'));
    if (!(error instanceof AlefError) || error.code !== 'PERMISSION_DENIED' || error.status !== 403) {
      throw new Error(`not denied: ${error ?? 'it succeeded'}`);
    }
    // the runner sets the variable to a marker value; the denial names the permission and nothing else
    if (error.message !== 'permission denied') throw new Error(`the denial says too much: ${error.message}`);
    if (JSON.stringify(error).includes('secret-value-31337')) throw new Error('the denial leaks the value');
  });
  await check('app-env-without-a-name-lists-only-the-listed-variables', async () => {
    const all = await api.app.env();
    if (!equal(all, { ALEF_E2E_ENV_ALLOWED: 'yes' })) throw new Error(`env ${JSON.stringify(all)}`);
  });
  await check('app-cwd-is-the-working-directory-of-the-process', async () => {
    const found = await api.app.cwd();
    const same = (a, b) => a.replaceAll('\\', '/').replace(/\/$/, '').toLowerCase() === b.replaceAll('\\', '/').replace(/\/$/, '').toLowerCase();
    if (!same(found, cwd)) throw new Error(`cwd ${found}, expected ${cwd}`);
  });
  await check('app-quit-rejects-a-code-outside-0-255', async () => {
    for (const code of [256, -1, 1.5]) {
      const error = await rejection(api.app.quit(code));
      if (!(error instanceof AlefError) || error.code !== 'INVALID_ARGUMENT') throw new Error(`${code}: ${error ?? 'it quit'}`);
    }
  });

  await verdict(failed());
  if (args.parsed.label === 'quit') await api.app.quit(7);
}

guard(main);
