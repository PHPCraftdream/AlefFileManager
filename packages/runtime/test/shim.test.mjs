// SPDX-License-Identifier: MIT OR Apache-2.0
import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, readFileSync, writeFileSync } from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { temporary } from './helpers.mjs';

const isWindows = process.platform === 'win32';
const shim = fileURLToPath(new URL('../bin/alef.mjs', import.meta.url));

// The fake "alef" executable must be spawnable WITHOUT a shell: modern Node
// refuses .cmd/.bat children (EINVAL), so on Windows the fake is
// process.execPath plus a driver script consumed as its first argument; on
// Unix it is a 0o755 shell script. Both echo what they received.
function fakeExecutable(t, unixBody, windowsBody) {
  const directory = temporary(t);
  if (isWindows) {
    const driver = join(directory, 'driver.cjs');
    writeFileSync(driver, windowsBody);
    return { bin: process.execPath, args: [driver] };
  }
  const file = join(directory, 'alef-fake');
  writeFileSync(file, `#!/bin/sh\n${unixBody}\n`);
  chmodSync(file, 0o755);
  return { bin: file, args: [] };
}

function runShim(t, fake, args, env = {}) {
  const result = spawnSync(process.execPath, [shim, ...fake.args, ...args], {
    encoding: 'utf8',
    timeout: 15_000,
    windowsHide: true,
    env: { ...process.env, ...env, ALEF_BIN: fake.bin },
  });
  assert.equal(result.error, undefined, `spawnSync failed: ${result.error}`);
  return result;
}

test('shim_forwards_arguments_verbatim_to_the_resolved_binary_without_a_shell', t => {
  const fake = fakeExecutable(t,
    'printf \'ARGS:\'; for a in "$@"; do printf \' <%s>\' "$a"; done; echo',
    'console.log("ARGS:" + JSON.stringify(process.argv.slice(2)));');
  const result = runShim(t, fake, ['run', '--flag', 'value with spaces', '--', '-x']);
  assert.equal(result.status, 0);
  const expected = isWindows
    ? 'ARGS:["run","--flag","value with spaces","--","-x"]'
    : 'ARGS: <run> <--flag> <value with spaces> <--> <-x>';
  assert.equal(result.stdout.trim(), expected);
  // Direct-spawn proof: the shim resolves ALEF_BIN and spawns that exact file
  // as the immediate child with no `shell` option, so argv arrives unsplit and
  // unquoted; on Unix the $0 and parent-pid checks below confirm no wrapping shell.
});

test('shim_forwards_the_child_exit_code', t => {
  const fake = fakeExecutable(t, 'exit 7', 'process.exit(7);');
  const result = runShim(t, fake, []);
  assert.equal(result.status, 7);
});

test('shim_reports_resolution_failure_on_stderr_with_exit_1_naming_alef_bin', t => {
  const fake = { bin: temporary(t), args: [] };
  const result = runShim(t, fake, []);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /alef: /);
  assert.match(result.stderr, /ALEF_BIN/);
  assert.equal(result.stdout, '');
});

test('non_executable_alef_bin_regular_file_resolves_but_fails_at_spawn_with_exit_1', t => {
  const data = join(temporary(t), 'data.bin');
  writeFileSync(data, 'plain data, not executable');
  const result = runShim(t, { bin: data, args: [] }, []);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /alef: /);
  // Documented rule: resolution only checks a regular file; the spawn failure
  // (ENOENT/EACCES error event, or a synchronous EINVAL/EFTYPE throw on
  // Windows) becomes `alef: <message>` on stderr and exit 1. No shell fallback.
});

test('shim_spawns_the_fake_binary_directly_not_through_cmd_exe_or_sh', t => {
  // Structural guarantee on every platform: the shim never passes a `shell`
  // option, so children are direct. On Unix this is additionally proven below:
  // the parent of the fake is the shim process itself.
  assert.ok(!readFileSync(shim, 'utf8').includes('shell:'),
    'shim must not pass a shell option to spawn');
  if (!isWindows) {
    const fake = fakeExecutable(t, 'echo "0=$0"; echo "PPID=$PPID"', '');
    const result = runShim(t, fake, []);
    assert.equal(result.status, 0);
    const lines = result.stdout.trim().split('\n');
    // $0 is the ABSOLUTE path of the fake script itself (not a shell), so the
    // line must end with the fake's file name.
    assert.match(lines.find(l => l.startsWith('0=')), /\/alef-fake$/, '$0 is the fake script itself, not a shell');
    assert.equal(lines.find(l => l.startsWith('PPID=')), `PPID=${result.pid}`, 'the shim is the immediate parent: no shell in between');
  }
});

test('shim_forwards_sigterm_to_a_running_child_and_exits_128_plus_15_on_unix', async t => {
  if (isWindows) {
    return t.skip('Windows Node cannot deliver real SIGINT/SIGTERM to the shim; forwarding is best-effort');
  }
  // Documented assertion set: after TERM the fake must install a trap, print
  // `got-term` and exit itself, and the shim must terminate with signal
  // SIGTERM or exit code 143 (a clean exit code, never above 143). Cleanup is
  // verified by PID: both the shim (whole process group) and the fake must be
  // gone (ESRCH) afterwards.
  const fake = fakeExecutable(t,
    'echo $$; trap \'echo got-term; exit 143\' TERM; echo ready; sleep 30 & wait $!; exit 0', '');
  const child = spawn(process.execPath, [shim], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, ALEF_BIN: fake.bin },
    detached: true, // own process group, so finally can kill the whole tree
  });
  let stdout = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  const groupAlive = group => {
    try { process.kill(-group, 0); return true; } catch (error) { return error.code !== 'ESRCH'; }
  };
  const waitFor = (predicate, label, ms = 10_000) => new Promise((resolve, reject) => {
    const start = Date.now();
    const timer = setInterval(() => {
      if (predicate()) { clearInterval(timer); resolve(); }
      else if (Date.now() - start > ms) { clearInterval(timer); reject(new Error(`${label}: ${stdout}`)); }
    }, 50);
  });
  let fakePid = Number.NaN;
  let outcome;
  try {
    await waitFor(() => stdout.includes('ready'), 'fake never became ready');
    fakePid = Number.parseInt(stdout.split('\n')[0], 10);
    assert.ok(Number.isInteger(fakePid), `fake pid missing from stdout: ${stdout}`);
    child.kill('SIGTERM');
    outcome = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`shim did not exit after SIGTERM: ${stdout}`)), 10_000);
      child.once('exit', (code, signal) => { clearTimeout(timer); resolve({ code, signal }); });
    });
    assert.ok(stdout.includes('got-term'), `fake never printed got-term: ${stdout}`);
    assert.deepEqual(outcome, { code: 143, signal: null }, 'the shim exits 128+15, not by a signal');
  } finally {
    // The shim, the fake and its `sleep` share one process group: kill it and require it gone.
    try { process.kill(-child.pid, 'SIGKILL'); } catch {}
    await waitFor(() => !groupAlive(child.pid), 'the process group did not fully die', 10_000);
    assert.throws(() => process.kill(-child.pid, 0), error => error.code === 'ESRCH', 'no process of the group is left');
  }
});

test('shim_maps_a_signal_killed_child_to_128_plus_signal_number_exiting_143', async t => {
  if (isWindows) {
    return t.skip('Windows Node cannot deliver real SIGINT/SIGTERM to the shim; forwarding is best-effort');
  }
  // Documented assertion set: the fake traps SIGTERM only to re-raise it
  // (`trap - TERM; kill -TERM $$`), so it dies BY SIGTERM and the shim's
  // 'close' fires with signal='SIGTERM'. The shim re-raises TERM to itself,
  // which its own forward-only handler swallows, so the signal-death branch's
  // explicit exit code is the effective outcome: 128 + 15 = 143, never 0 and
  // never above 143 (this kills a `process.exit(0)` mutant). Cleanup is
  // verified by PID: the shim (whole process group — the fake runs in it,
  // spawned non-detached) and the fake are gone (ESRCH) afterwards.
  const fake = fakeExecutable(t,
    'trap \'trap - TERM; kill -TERM $$\' TERM; echo $$; echo ready; sleep 30 & wait $!; exit 0', '');
  const child = spawn(process.execPath, [shim], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, ALEF_BIN: fake.bin },
    detached: true, // own process group, so finally can kill the whole tree
  });
  let stdout = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  const groupAlive = group => {
    try { process.kill(-group, 0); return true; } catch (error) { return error.code !== 'ESRCH'; }
  };
  const waitFor = (predicate, label, ms = 10_000) => new Promise((resolve, reject) => {
    const start = Date.now();
    const timer = setInterval(() => {
      if (predicate()) { clearInterval(timer); resolve(); }
      else if (Date.now() - start > ms) { clearInterval(timer); reject(new Error(`${label}: ${stdout}`)); }
    }, 50);
  });
  let fakePid = Number.NaN;
  let outcome;
  try {
    await waitFor(() => stdout.includes('ready'), 'fake never became ready');
    fakePid = Number.parseInt(stdout.split('\n')[0], 10);
    assert.ok(Number.isInteger(fakePid), `fake pid missing from stdout: ${stdout}`);
    // TERM the fake directly (it runs in the shim's group, which must stay
    // untouched): the trap re-raises, so the fake dies BY SIGTERM.
    process.kill(fakePid, 'SIGTERM');
    outcome = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`shim did not exit after the fake was TERMed: ${stdout}`)), 10_000);
      child.once('exit', (code, signal) => { clearTimeout(timer); resolve({ code, signal }); });
    });
    assert.deepEqual(outcome, { code: 143, signal: null }, 'the shim exits 128+15, not by a signal');
  } finally {
    // The shim, the fake and its `sleep` share one process group: kill it and require it gone.
    try { process.kill(-child.pid, 'SIGKILL'); } catch {}
    await waitFor(() => !groupAlive(child.pid), 'the process group did not fully die', 10_000);
    assert.throws(() => process.kill(-child.pid, 0), error => error.code === 'ESRCH', 'no process of the group is left');
  }
});
