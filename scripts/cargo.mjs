import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const [command, ...arguments_] = process.argv.slice(2);
if (!command) throw new Error('A Cargo command is required.');
const result = spawnSync('cargo', [command, '--manifest-path', 'backend/Cargo.toml',
  ...(command === 'fmt' ? [] : ['--jobs', '1']), ...arguments_], {
  cwd: root,
  stdio: 'inherit',
  env: { ...process.env, RUSTC_WRAPPER: '' },
});
if (result.error) throw result.error;
process.exit(result.status ?? 1);
