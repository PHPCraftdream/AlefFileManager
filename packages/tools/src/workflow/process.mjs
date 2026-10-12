// SPDX-License-Identifier: MIT OR Apache-2.0
import { spawn } from 'node:child_process';

export function validateTimeout(timeoutMs) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 2147483647) throw new Error('timeoutMs must be a positive bounded integer');
}
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const isAlive = pid => { try { process.kill(pid, 0); return true; } catch { return false; } };
/** Bounded liveness check: the tree must be gone, or stop() failed. */
async function assertTreeDead(child) {
  const deadline = Date.now() + 3000;
  while (isAlive(child.pid) && Date.now() < deadline) await delay(50);
  if (isAlive(child.pid)) throw new Error(`failed to stop owned process tree <pid ${child.pid}>`);
}
/** Only kill a PID/group created by this module, never a discovered server. */
export async function killTree(child) {
  if (!child.pid) return;
  if (process.platform === 'win32') {
    const code = await new Promise((resolve, reject) => {
      const killer = spawn('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
      const timer = setTimeout(() => { killer.kill(); reject(new Error('taskkill exceeded cleanup timeout')); }, 5000);
      killer.once('error', error => { clearTimeout(timer); reject(error); });
      // Nonzero (128 not found, 255 lost race, 1 access denied) is not trusted:
      // accept only after the bounded liveness check below proves the tree died.
      killer.once('close', exit => { clearTimeout(timer); resolve(exit); });
    });
    if (code !== 0) await assertTreeDead(child);
    return;
  }
  const signal = name => { try { process.kill(-child.pid, name); } catch (error) { if (error.code !== 'ESRCH') throw error; } };
  signal('SIGTERM'); await delay(100); signal('SIGKILL');
  await assertTreeDead(child);
}
/** Exact author shell string, or fixed npm argv; owned Unix process group. */
export function startCommand(command, { cwd, stdio = 'inherit' } = {}) {
  if (!command || (command.npm !== undefined && !['build', 'dev'].includes(command.npm))) throw new Error('Invalid command');
  const windows = process.platform === 'win32';
  let executable; let args;
  if (command.npm) {
    executable = windows ? (process.env.ComSpec || 'cmd.exe') : 'npm';
    args = windows ? ['/d', '/s', '/c', `npm run ${command.npm}`] : ['run', command.npm];
  } else {
    if (typeof command.shell !== 'string' || !command.shell.trim()) throw new Error('Invalid shell command');
    executable = windows ? (process.env.ComSpec || 'cmd.exe') : '/bin/sh';
    args = windows ? ['/d', '/s', '/c', `"${command.shell}"`] : ['-c', command.shell];
  }
  return spawn(executable, args, { cwd, stdio, detached: !windows, windowsHide: true, windowsVerbatimArguments: windows });
}
export async function runCommand(command, { cwd, timeoutMs = 600000, stdio = 'inherit' } = {}) {
  validateTimeout(timeoutMs);
  const child = startCommand(command, { cwd, stdio });
  let timer; let cleanup; let failure;
  const stop = () => cleanup ??= killTree(child);
  try {
    await new Promise((resolve, reject) => {
      timer = setTimeout(() => {
        failure = new Error(`Command timed out after ${timeoutMs} ms`);
        stop().then(() => reject(failure), reject);
      }, timeoutMs);
      child.once('error', reject);
      // exit rather than close: grandchildren may retain inherited pipe handles.
      child.once('exit', (code, signal) => {
        if (failure) return;
        if (code === 0) resolve(); else reject(new Error(`Command failed (exit ${code}, signal ${signal ?? 'none'})`));
      });
    });
  } finally { clearTimeout(timer); await stop(); }
}
