import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = fileURLToPath(new URL('../', import.meta.url));
const executable = path.join(root, 'backend', 'target', 'debug',
  process.platform === 'win32' ? 'alef-file-manager.exe' : 'alef-file-manager');
if (!existsSync(executable)) throw new Error('Run npm run build before npm start.');
const child = spawn(executable, process.argv.slice(2), {
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
