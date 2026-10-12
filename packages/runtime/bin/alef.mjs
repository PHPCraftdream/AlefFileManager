#!/usr/bin/env node
// SPDX-License-Identifier: MIT OR Apache-2.0
// Alef launcher shim: resolves the platform runtime binary and executes it
// directly (never through a shell), forwarding arguments, exit codes and signals.
import { spawn } from 'node:child_process';
import os from 'node:os';
import { getRuntimePath } from '../src/index.mjs';

let binary;
try {
  binary = getRuntimePath();
} catch (error) {
  console.error(`alef: ${error.message}`);
  process.exitCode = 1;
}
if (binary !== undefined) {
  const args = process.argv.slice(2);
  // stdio: 'inherit' and no shell option: the binary is always a direct child.
  // Spawn failures (ENOENT, EACCES and, on Windows, EFTYPE/EINVAL for
  // non-executable files such as a plain-data ALEF_BIN) either throw
  // synchronously here or surface as the 'error' event below; both print
  // `alef: <message>` and exit 1.
  let child;
  try {
    child = spawn(binary, args, { stdio: 'inherit' });
  } catch (error) {
    console.error(`alef: ${error.message}`);
    process.exit(1);
  }
  if (child !== undefined) {
  child.on('error', error => {
    // Spawn failures (ENOENT, EACCES, ...) surface here, including the
    // case where ALEF_BIN points to a non-executable regular file.
    console.error(`alef: ${error.message}`);
    process.exit(1);
  });
  child.on('close', (code, signal) => {
    // A child killed by a signal ends the shim with the shell convention 128+n.
    if (signal !== null) process.exit(128 + (os.constants.signals[signal] ?? 0));
    process.exit(code ?? 0);
  });

  if (process.platform !== 'win32') {
    for (const name of ['SIGINT', 'SIGTERM']) {
      // Forward to the live child only; the shim itself exits in the
      // 'close' handler above, so no exited-state tracking is needed here.
      process.on(name, signal => {
        try {
          child.kill(signal);
        } catch (error) {
          if (error.code !== 'ESRCH') throw error;
        }
      });
    }
  }
  // Windows note: Node cannot deliver real SIGINT/SIGTERM to this process, so
  // forwarding is only possible on POSIX; on Windows rely on inherited stdio
  // and console control handling of the child itself.
  }
}
