// SPDX-License-Identifier: MIT OR Apache-2.0
// Runs `cargo test` for each crate of the plan, one after another; stops at the first failure.
// Shell-agnostic on purpose: the same step runs under bash and PowerShell.
//   node scripts/ci/test-crates.mjs "alef,alef-runtime"
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export function parseCrates(list) {
  const seen = new Set();
  for (const name of String(list ?? '').split(/[\s,]+/)) {
    if (name && /^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(name)) seen.add(name);
  }
  return [...seen];
}

function main() {
  const crates = parseCrates(process.argv[2]);
  const cargo = fileURLToPath(new URL('../cargo.mjs', import.meta.url));
  for (const crate of crates) {
    const run = spawnSync(process.execPath, [cargo, 'test', '--locked', '-p', crate, '--no-fail-fast'], { stdio: 'inherit' });
    if (run.status !== 0) process.exit(run.status ?? 1);
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main();
