import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = fileURLToPath(new URL('../', import.meta.url));
const executable = path.join(root, 'backend', 'target', 'debug', process.platform === 'win32' ? 'alef.exe' : 'alef');
const application = path.join(root, 'frontend', 'dist');
if (!existsSync(executable) || !existsSync(path.join(application, 'alef.ktav'))) throw new Error('Run npm run build before npm start.');
// Everything after the script name is the command line of the application (for example --root DIRECTORY).
const given = process.argv.slice(2);
const child = spawn(executable, ['--app', application, ...(given.length > 0 ? ['--', ...given] : [])], {
  cwd: root,
  stdio: 'inherit',
  detached: true,
  windowsHide: true,
});
child.once('error', error => {
  console.error(error);
  process.exitCode = 1;
});
child.unref();
console.log(`Started AlefFileManager (PID ${child.pid}).`);
