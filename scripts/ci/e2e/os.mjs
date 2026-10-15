// SPDX-License-Identifier: MIT OR Apache-2.0
// Grouped e2e for one operating system: runs every canonical group (scripts/ci/e2e/groups.mjs)
// whose scenarios the plan selected, each in its own runner process; a failing group does not
// stop the others; the exit code is 1 when any group failed. A group's deadline is
// selected.length * timeout-s * 1000 + ALEF_E2E_GROUP_MARGIN_MS: every scenario gets its own
// --timeout-s ceiling, the margin covers startup, the transpile and the runner itself; an
// overdue group fails and the next group still runs.
//   node scripts/ci/e2e/os.mjs --plan "<plan e2e>" --exe <path> [--timeout-s 150]
// Env ALEF_E2E_GROUP_RUNNER (test hook, never set by CI) points at a stub runner script;
// ALEF_E2E_GROUP_MARGIN_MS (test hook, never set by CI) widens the per-group deadline.

import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { selectGroup } from './group.mjs';
import { E2E_GROUPS } from './groups.mjs';

function usage(message) {
  console.error(message);
  console.error('usage: node scripts/ci/e2e/os.mjs --plan <plan e2e> --exe <path> [--timeout-s <n>]');
}

function parseArgs(argv) {
  const args = { '--timeout-s': '150' };
  const known = ['--plan', '--exe', '--timeout-s'];
  for (let i = 0; i < argv.length; i += 2) {
    const flag = argv[i];
    if (!known.includes(flag) || i + 1 >= argv.length) {
      usage(`unknown or incomplete argument: ${flag}`);
      process.exit(1);
    }
    args[flag] = argv[i + 1];
  }
  for (const flag of ['--plan', '--exe']) {
    if (args[flag] === undefined) {
      usage(`missing required argument: ${flag}`);
      process.exit(1);
    }
  }
  return args;
}

async function run() {
  const args = parseArgs(process.argv.slice(2));
  const timeout = String(Number(args['--timeout-s']));
  const runner = process.env.ALEF_E2E_GROUP_RUNNER
    ? path.resolve(process.env.ALEF_E2E_GROUP_RUNNER)
    : fileURLToPath(new URL('../../../tests/e2e/run.mjs', import.meta.url));
  const failed = [];
  for (const group of E2E_GROUPS) {
    const selected = selectGroup(args['--plan'], group);
    if (selected.length === 0) {
      console.log(`group ${group}: nothing planned`);
      continue;
    }
    // Test hook, never set by CI: slack on top of the per-scenario ceilings.
    const marginMs = Number(process.env.ALEF_E2E_GROUP_MARGIN_MS) || 120000;
    const deadlineMs = selected.length * Number(timeout) * 1000 + marginMs;
    const outcome = await new Promise(resolve => {
      const child = spawn(process.execPath, [
        runner,
        '--exe', args['--exe'],
        '--timeout-s', timeout,
        '--verbose',
        '--only', selected.join(','),
      ], { stdio: 'inherit', detached: process.platform !== 'win32' });
      let timedOut = false;
      const timer = setTimeout(() => {
        timedOut = true;
        if (process.platform === 'win32') {
          spawn('taskkill', ['/pid', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
        } else {
          try { process.kill(-child.pid, 'SIGKILL'); } catch { child.kill('SIGKILL'); }
        }
      }, deadlineMs);
      child.on('error', error => {
        clearTimeout(timer);
        console.error(`group ${group}: spawn failed: ${error?.message ?? String(error)}`);
        resolve({ timedOut: false, code: 1 });
      });
      child.on('exit', code => {
        clearTimeout(timer);
        resolve({ timedOut, code: code ?? 1 });
      });
    });
    if (outcome.timedOut) {
      console.log(`group ${group}: FAIL (timeout after ${Math.round(deadlineMs / 1000)} s)`);
      failed.push(group);
    } else if (outcome.code === 0) {
      console.log(`group ${group}: PASS`);
    } else {
      console.log(`group ${group}: FAIL (exit ${outcome.code})`);
      failed.push(group);
    }
  }
  if (failed.length > 0) {
    console.error(`e2e groups failed: ${failed.join(', ')}`);
    process.exitCode = 1;
  } else {
    console.log('e2e: every planned group passed');
  }
}

const isMain = process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) run();
