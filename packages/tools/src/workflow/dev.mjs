// SPDX-License-Identifier: MIT OR Apache-2.0
import { spawn } from 'node:child_process';
import { accessSync, constants } from 'node:fs';
import path from 'node:path';
import { readProject } from './project.mjs';
import { killTree, startCommand, validateTimeout } from './process.mjs';

const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const owned = new Set();
/** PIDs spawned by this module and not yet cleaned up; exposed for tests. */
export const ownedPids = () => [...owned];
function own(child) { if (child.pid) owned.add(child.pid); return child; }
async function stop(child) {
  if (!child || child.stopped) return;
  child.stopped = true;
  await killTree(child);
  if (child.pid) owned.delete(child.pid);
}

/** Runtime executable resolution order: ALEF_BIN, @alef-runtime/runtime, PATH. */
export async function findRuntime(dir = process.cwd(), overrides = {}) {
  const fromEnv = overrides.alefBin ?? process.env.ALEF_BIN;
  if (fromEnv) return fromEnv;
  try {
    const runtime = await import(overrides.runtimeModule ?? '@alef-runtime/runtime');
    if (typeof runtime.getRuntimePath === 'function') {
      const found = await runtime.getRuntimePath(dir);
      if (found) return found;
    }
  } catch { /* the runtime package is optional and may be absent */ }
  const names = process.platform === 'win32' ? ['alef.exe', 'alef'] : ['alef'];
  for (const directory of (overrides.path ?? process.env.PATH ?? '').split(process.platform === 'win32' ? ';' : ':')) {
    if (!directory) continue;
    for (const name of names) {
      const candidate = path.join(directory, name);
      try { accessSync(candidate, constants.X_OK); return candidate; } catch { /* keep scanning */ }
    }
  }
  throw new Error('Alef runtime executable not found: set ALEF_BIN, install @alef-runtime/runtime exporting getRuntimePath(), or put alef on PATH');
}

/** Argv-only spawn; Windows needs a fixed ComSpec argv for .cmd/.bat shims, never an interpolated string. */
const unsafeForCmd = character => ' &|<>^%!"\r\n'.includes(character);
function runtimeSpawn(executable, args, options) {
  if (process.platform === 'win32' && /\.(cmd|bat)$/i.test(executable)) {
    for (const argument of args) {
      const bad = [...argument].find(unsafeForCmd);
      if (bad !== undefined) {
        throw new Error(`Unsafe runtime argument for a .cmd/.bat executable (cmd.exe metacharacter ${bad === '\r' ? '\\r' : bad === '\n' ? '\\n' : bad}); use a .exe or remove the character: ${argument}`);
      }
    }
    return spawn(process.env.ComSpec || 'cmd.exe', ['/d', '/s', '/c', executable, ...args],
      { ...options, shell: false, windowsVerbatimArguments: true });
  }
  return spawn(executable, args, { ...options, shell: false, detached: process.platform !== 'win32' });
}

const describeDevExit = (exit, failure) =>
  `Dev server exited before readiness (exit ${exit?.code}, signal ${exit?.signal ?? 'none'}${failure ? `: ${failure.message}` : ''})`;

/** CLI devtools option to runtime argv: true → `--devtools`, port → `--devtools=<port>`, else nothing. */
export function devtoolsArg(devtools) {
  if (devtools === true) return ['--devtools'];
  if (typeof devtools === 'number') return [`--devtools=${devtools}`];
  return [];
}

/** Start the dev command, wait for devUrl, then run the runtime until it exits. */
export async function dev(dir = process.cwd(), options = {}) {
  const readyTimeoutMs = options.readyTimeoutMs ?? 60000;
  validateTimeout(readyTimeoutMs);
  const project = await readProject(dir);
  if (!project.dev) throw new Error('package.json: no dev command or scripts.dev');
  if (!project.devUrl) throw new Error('package.json: a dev command without alef-runtime.devUrl is not supported');
  const devProcess = own(startCommand(project.dev, { cwd: project.root, stdio: options.devStdio ?? 'inherit' }));
  let devFailure; let devExit;
  devProcess.once('error', error => { devFailure = error; });
  devProcess.once('exit', (code, signal) => { devExit = { code, signal }; });
  const runtime = { stopped: false, child: undefined };
  // Cancellable signal handling: record the signal, fix the exit code (128+n),
  // and reject the ongoing wait so cleanup happens on the rejection path.
  let interrupted; let rejectWait;
  const interruptedError = () => new Error(`Interrupted by ${interrupted}`);
  const onSignal = name => {
    interrupted ??= name;
    process.exitCode ??= name === 'SIGINT' ? 130 : 143;
    rejectWait?.(interruptedError());
  };
  process.on('SIGINT', onSignal); process.on('SIGTERM', onSignal);
  let cancel; // deferred rejection raced against the current await
  const cancellable = promise => {
    cancel = new Promise((_, reject) => { rejectWait = reject; });
    cancel.catch(() => {}); // never an unhandled rejection
    return Promise.race([promise, cancel]);
  };
  let result; let failure;
  try {
    const deadline = Date.now() + readyTimeoutMs;
    for (;;) {
      if (interrupted) throw interruptedError();
      if (devFailure || devExit) throw new Error(describeDevExit(devExit, devFailure));
      let answered = false;
      try {
        const response = await fetch(project.devUrl, { signal: AbortSignal.timeout(1000) });
        answered = true; response.body?.cancel().catch(() => {});
      } catch { /* not ready yet; keep polling */ }
      if (answered) break;
      if (Date.now() >= deadline) throw new Error(`Dev server at ${project.devUrl} was not ready within ${readyTimeoutMs} ms`);
      await cancellable(delay(250));
    }
    if (interrupted) throw interruptedError();
    const executable = await findRuntime(project.root, options);
    const runtimeArgv = ['--app', project.root, '--dev-url', project.devUrl,
      ...devtoolsArg(options.devtools),
      ...(options.appArgs?.length ? ['--', ...options.appArgs] : [])];
    runtime.child = own(runtimeSpawn(executable, runtimeArgv,
      { stdio: 'inherit', windowsHide: true }));
    let runtimeFailure;
    let code;
    try {
      code = await cancellable(new Promise((resolve, reject) => {
        runtime.child.once('error', error => { runtimeFailure = error; reject(error); });
        // When interrupted, the rejection path wins: never resolve with a null code.
        runtime.child.once('exit', exitCode => { if (!runtimeFailure && !interrupted) resolve(exitCode); });
        devProcess.once('exit', (exitCode, signal) => {
          if (!runtimeFailure && !interrupted && runtime.child.exitCode === null) {
            reject(new Error(`Dev server died while the runtime was running (exit ${exitCode}, signal ${signal ?? 'none'})`));
          }
        });
      }));
    } catch (error) {
      if (runtimeFailure) throw new Error(`Runtime ${executable} failed to start: ${error.message}`, { cause: error });
      throw error;
    }
    if (interrupted) throw interruptedError();
    result = Object.freeze({ code });
  } catch (error) {
    failure = error;
  } finally {
    for (const name of ['SIGINT', 'SIGTERM']) process.removeListener(name, onSignal);
    rejectWait = undefined; // further signals only record state; cleanup runs once below
    await Promise.allSettled([stop(devProcess), stop(runtime.child)]);
  }
  if (interrupted) throw interruptedError(); // the rejection path wins over any result or error
  if (failure) throw failure;
  return result;
}
