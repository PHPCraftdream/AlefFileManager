import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const root = fileURLToPath(new URL('../', import.meta.url));
const children = new Set();
let stopping = false;

function stopChild(child) {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === 'win32') {
    spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
  } else {
    process.kill(-child.pid, 'SIGTERM');
  }
}

function shutdown() {
  if (stopping) return;
  stopping = true;
  for (const child of children) stopChild(child);
}

process.once('SIGINT', shutdown);
process.once('SIGTERM', shutdown);
process.once('exit', shutdown);

function launch(command, args, env = process.env) {
  const child = spawn(command, args, { cwd: root, env, stdio: 'inherit',
    detached: process.platform !== 'win32', windowsHide: true });
  children.add(child);
  child.once('close', () => children.delete(child));
  return child;
}

function finished(child) {
  return new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (code, signal) => {
      if (code === 0) resolve();
      else reject(new Error(`Child process exited: ${code ?? signal}`));
    });
  });
}

try {
  const build = launch('cargo', ['build', '--manifest-path', 'backend/Cargo.toml',
    '--locked', '--jobs', '1'], { ...process.env, RUSTC_WRAPPER: '' });
  await finished(build);
  if (stopping) process.exit(0);
  const frontend = launch(process.execPath,
    [path.join(root, 'node_modules', '@rsbuild', 'core', 'bin', 'rsbuild.js'), 'dev']);
  frontend.once('error', error => {
    if (!stopping) { console.error(error); process.exitCode = 1; shutdown(); }
  });
  frontend.once('close', (code, signal) => {
    if (!stopping) {
      console.error(`Rsbuild exited before application shutdown: ${code ?? signal}`);
      process.exitCode = code || 1;
      shutdown();
    }
  });
  const deadline = Date.now() + 30_000;
  let ready = false;
  while (Date.now() < deadline) {
    if (stopping || frontend.exitCode !== null) break;
    try {
      const response = await fetch('http://127.0.0.1:3000/', { signal: AbortSignal.timeout(1000) });
      if (response.ok) { ready = true; break; }
    } catch (error) {
      if (!(error instanceof TypeError) && !(error instanceof DOMException)) throw error;
    }
    await delay(100);
  }
  if (!ready) throw new Error('Rsbuild did not start on 127.0.0.1:3000.');
  const executable = path.join(root, 'backend', 'target', 'debug',
    process.platform === 'win32' ? 'alef-file-manager.exe' : 'alef-file-manager');
  const application = launch(executable,
    ['--frontend-url', 'http://127.0.0.1:3000/', ...process.argv.slice(2)]);
  await finished(application);
} catch (error) {
  if (!stopping) {
    console.error(error);
    process.exitCode = 1;
  }
} finally {
  shutdown();
}
