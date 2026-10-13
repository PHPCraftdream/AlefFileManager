// SPDX-License-Identifier: MIT OR Apache-2.0
// e2e group selector: intersects the plan's e2e list with one fixed Windows group and calls
// tests/e2e/run.mjs; empty intersection prints a note and exits 0.
//   node scripts/ci/e2e-group.mjs --plan "<plan e2e>" --group a,b,c --exe <path> [--timeout-s 150]
// Env ALEF_E2E_GROUP_RUNNER (test hook, never set by CI) points at a stub runner script.

import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

// Group scenarios present in the plan; 'all' selects the whole group; ''/'none' select nothing.
// Order and dedup follow the group, not the plan; unknown plan names are ignored.
export function selectGroup(plan, group) {
  const list = group.split(',').map(s => s.trim()).filter(Boolean);
  const wanted = plan.trim().split(',').map(s => s.trim()).filter(Boolean);
  if (plan.trim() === 'all') return [...new Set(list)];
  if (plan.trim() === '' || plan.trim() === 'none') return [];
  const planned = new Set(wanted);
  return [...new Set(list.filter(s => planned.has(s)))];
}

function usage(message) {
  console.error(message);
  console.error('usage: node scripts/ci/e2e-group.mjs --plan <plan e2e> --group <comma list> --exe <path> [--timeout-s <n>]');
}

function parseArgs(argv) {
  const args = { '--timeout-s': '150' };
  const known = ['--plan', '--group', '--exe', '--timeout-s'];
  for (let i = 0; i < argv.length; i += 2) {
    const flag = argv[i];
    if (!known.includes(flag) || i + 1 >= argv.length) {
      usage(`unknown or incomplete argument: ${flag}`);
      process.exit(1);
    }
    args[flag] = argv[i + 1];
  }
  for (const flag of ['--plan', '--group', '--exe']) {
    if (args[flag] === undefined) {
      usage(`missing required argument: ${flag}`);
      process.exit(1);
    }
  }
  return args;
}

function run() {
  const args = parseArgs(process.argv.slice(2));
  const selected = selectGroup(args['--plan'], args['--group']);
  if (selected.length === 0) {
    console.log('no planned scenario in this group');
    return;
  }
  const runner = process.env.ALEF_E2E_GROUP_RUNNER
    ? path.resolve(process.env.ALEF_E2E_GROUP_RUNNER)
    : fileURLToPath(new URL('../../tests/e2e/run.mjs', import.meta.url));
  const child = spawn(process.execPath, [
    runner,
    '--exe', args['--exe'],
    '--timeout-s', String(Number(args['--timeout-s'])),
    '--verbose',
    '--only', selected.join(','),
  ], { stdio: 'inherit' });
  child.on('error', error => {
    console.error(`spawn failed: ${error?.message ?? String(error)}`);
    process.exit(1);
  });
  child.on('exit', code => process.exit(code ?? 1));
}

const isMain = process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) run();
