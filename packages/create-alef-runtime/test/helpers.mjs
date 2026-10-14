// SPDX-License-Identifier: MIT OR Apache-2.0
// Shared helpers: temp directories with cleanup and bounded child-process runs.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const binPath = fileURLToPath(new URL('../bin/create-alef-runtime.mjs', import.meta.url));

export async function scratch(prefix = 'create-alef-test-') {
  const root = await mkdtemp(path.join(tmpdir(), prefix));
  return { root, async [Symbol.asyncDispose]() { await rm(root, { recursive: true, force: true }); } };
}

export function runCli(args, cwd, { input = null } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [binPath, ...args], { cwd, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = ''; let stderr = '';
    const fail = message => { error ??= new Error(message); child.kill('SIGKILL'); };
    let error;
    const timer = setTimeout(() => fail('create-alef-runtime child exceeded its 30s timeout'), 30_000);
    child.stdout.on('data', bytes => { stdout += bytes; if (stdout.length > 65_536) fail('child stdout exceeded its bound'); });
    child.stderr.on('data', bytes => { stderr += bytes; if (stderr.length > 65_536) fail('child stderr exceeded its bound'); });
    if (input !== null) child.stdin.end(input); else child.stdin.end();
    child.on('error', err => { error = err; });
    child.on('close', (code, signal) => {
      clearTimeout(timer);
      if (error) reject(error); else resolve({ code, signal, stdout, stderr });
    });
  });
}

export function expectSuccess(result) {
  assert.equal(result.signal, null);
  assert.equal(result.code, 0, `stderr: ${result.stderr}`);
  return result.stdout;
}

export function expectFailure(result, pattern) {
  assert.equal(result.signal, null);
  assert.equal(result.code, 1);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, pattern);
}

export async function importToolsCheck() {
  const tools = await import(pathToFileURL(path.join(import.meta.dirname, '../../tools/src/index.mjs')));
  return tools.check;
}

export { assert };
