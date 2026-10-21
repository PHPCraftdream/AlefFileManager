// SPDX-License-Identifier: MIT OR Apache-2.0
// Licenses of the npm dependencies against the list the Rust side uses (backend/deny.toml, [licenses] allow):
//   node scripts/supply/check-js-licenses.mjs [package-lock.json] [backend/deny.toml]
// One list for both ecosystems: `A OR B` passes when one side is on it, `A AND B` when both are,
// `X WITH e` when the pair or X alone is (an exception only widens what X grants).

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

// The [licenses] allow list of a cargo-deny configuration, lower-cased (SPDX ids are case-insensitive).
export function allowedLicenses(toml) {
  const lines = toml.split(/\r?\n/).map(line => line.replace(/#.*$/, ''));
  const start = lines.findIndex(line => line.trim() === '[licenses]');
  if (start < 0) throw new Error('no [licenses] section');
  const next = lines.findIndex((line, index) => index > start && /^\s*\[/.test(line));
  const body = lines.slice(start + 1, next < 0 ? lines.length : next).join('\n');
  const list = body.match(/^\s*allow\s*=\s*\[([^\]]*)\]/m);
  if (!list) throw new Error('no allow list in [licenses]');
  const ids = [...list[1].matchAll(/"([^"]+)"/g)].map(match => match[1].toLowerCase());
  if (ids.length === 0) throw new Error('the allow list of [licenses] is empty');
  return new Set(ids);
}

// Whether an SPDX expression is satisfied by the allowed ids; throws on an expression it cannot read.
export function satisfied(expression, allowed) {
  const tokens = expression.match(/\(|\)|[^\s()]+/g) ?? [];
  let at = 0;
  const isWord = (token, word) => token !== undefined && token.toUpperCase() === word;

  function atom() {
    const name = tokens[at++];
    if (name === undefined || name === ')' || isWord(name, 'OR') || isWord(name, 'AND') || isWord(name, 'WITH')) {
      throw new Error(`unexpected ${name ?? 'end of the expression'}`);
    }
    if (name === '(') {
      const inner = anyOf();
      if (tokens[at++] !== ')') throw new Error('unclosed parenthesis');
      return inner;
    }
    if (isWord(tokens[at], 'WITH')) {
      at++;
      const exception = tokens[at++];
      if (exception === undefined) throw new Error('WITH without an exception');
      return allowed.has(`${name} WITH ${exception}`.toLowerCase()) || allowed.has(name.toLowerCase());
    }
    return allowed.has(name.toLowerCase());
  }

  function allOf() {
    let all = atom();
    while (isWord(tokens[at], 'AND')) {
      at++;
      const rest = atom();
      all = all && rest;
    }
    return all;
  }

  function anyOf() {
    let any = allOf();
    while (isWord(tokens[at], 'OR')) {
      at++;
      const rest = allOf();
      any = any || rest;
    }
    return any;
  }

  const verdict = anyOf();
  if (at < tokens.length) throw new Error(`unexpected ${tokens[at]}`);
  return verdict;
}

// Every package of a lock file (the root and links excluded) against the allowed ids.
export function checkLock(lock, allowed) {
  const failures = [];
  let checked = 0;
  for (const [location, entry] of Object.entries(lock.packages ?? {})) {
    if (location === '' || entry.link) continue;
    checked++;
    const label = `${entry.name ?? location.replace(/^.*node_modules\//, '')}@${entry.version ?? '?'}`;
    const license = entry.license;
    if (typeof license !== 'string' || license.trim() === '') {
      failures.push(`${label}: no license in the lock file`);
      continue;
    }
    let passes = false;
    try {
      passes = satisfied(license, allowed);
    } catch (error) {
      failures.push(`${label}: cannot read the license "${license}" (${error.message})`);
      continue;
    }
    if (!passes) failures.push(`${label}: ${license}`);
  }
  return { checked, failures };
}

function main(argv) {
  const root = fileURLToPath(new URL('../../', import.meta.url));
  const [lockPath = `${root}package-lock.json`, denyPath = `${root}backend/deny.toml`] = argv;
  let allowed;
  let lock;
  try {
    allowed = allowedLicenses(readFileSync(denyPath, 'utf8'));
    lock = JSON.parse(readFileSync(lockPath, 'utf8'));
  } catch (error) {
    console.error(`cannot read the inputs: ${error.message}`);
    return 2;
  }
  const { checked, failures } = checkLock(lock, allowed);
  console.log(`${checked} npm packages against ${allowed.size} allowed licenses`);
  for (const failure of failures) console.error(`not allowed: ${failure}`);
  return failures.length === 0 ? 0 : 1;
}

if (process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url)) {
  process.exitCode = main(process.argv.slice(2));
}
