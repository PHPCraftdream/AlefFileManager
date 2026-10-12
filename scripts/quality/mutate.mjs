// Mutation-testing harness: apply one source mutation at a time and classify the test outcome.
// Usage: node scripts/quality/mutate.mjs <list.json|list-name> [--shard i/n] [--filter "regex"]
import { readFileSync, writeFileSync, appendFileSync } from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { pathToFileURL, fileURLToPath } from 'node:url';

export const OUTCOMES = ['CAUGHT', 'ESCAPED', 'INVALID', 'TIMEOUT', 'ANCHOR_MISSING', 'ANCHOR_AMBIGUOUS'];
const DEFAULT_TIMEOUT_MS = 45 * 60 * 1000;

export function parseArgv(argv) {
  const parsed = { list: null, shard: null, filter: null };
  for (let i = 0; i < argv.length; i++) {
    const argument = argv[i];
    if (argument === '--shard' || argument === '--filter') {
      const value = argv[++i];
      if (value === undefined) throw new Error(`${argument} needs a value`);
      parsed[argument.slice(2)] = value;
    } else if (argument.startsWith('--')) {
      throw new Error(`unknown option ${argument}`);
    } else if (parsed.list === null) {
      parsed.list = argument;
    } else {
      throw new Error(`unexpected argument ${argument}`);
    }
  }
  if (parsed.list === null) throw new Error('usage: mutate.mjs <list.json|list-name> [--shard i/n] [--filter "regex"]');
  return parsed;
}

export function parseShard(value) {
  const match = /^(\d+)\/(\d+)$/.exec(value);
  if (!match) throw new Error(`--shard must look like 2/4, got ${JSON.stringify(value)}`);
  const i = Number(match[1]);
  const n = Number(match[2]);
  if (n < 1 || i < 1 || i > n) throw new Error(`--shard needs 1 <= i <= n and n >= 1, got ${value}`);
  return { i, n };
}

export function validateList(list) {
  if (!Array.isArray(list)) throw new Error('the mutation list must be a JSON array');
  if (list.length === 0) throw new Error('the mutation list is empty');
  const required = ['name', 'file', 'from', 'to', 'package'];
  for (const [index, entry] of list.entries()) {
    const label = `mutation[${index}] (${JSON.stringify(entry?.name ?? null)})`;
    if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) throw new Error(`${label}: must be an object`);
    for (const key of Object.keys(entry)) {
      if (!required.includes(key) && key !== 'features' && key !== 'test') throw new Error(`${label}: unknown key ${JSON.stringify(key)}`);
    }
    for (const key of required) {
      if (typeof entry[key] !== 'string') throw new Error(`${label}: ${JSON.stringify(key)} must be a string`);
    }
    if (entry.features !== undefined && typeof entry.features !== 'string') throw new Error(`${label}: "features" must be a string`);
    if (entry.test !== undefined) {
      if (!Array.isArray(entry.test) || entry.test.some(t => typeof t !== 'string')) throw new Error(`${label}: "test" must be an array of strings`);
    }
  }
  return list;
}

export function loadList(list, root) {
  const file = list.endsWith('.json') ? path.join(root, list) : path.join(root, 'scripts', 'quality', 'mutations', `${list}.json`);
  return validateList(JSON.parse(readFileSync(file, 'utf8')));
}

/** Shard i (1-based) of n keeps the mutations whose original index k satisfies k % n === i - 1. */
export function selectShard(list, i, n) {
  return list.filter((_, k) => k % n === i - 1);
}

function anchorFor(text, from) {
  if (text.includes('\r') && !from.includes('\r')) return from.replaceAll('\n', '\r\n');
  if (!text.includes('\r') && from.includes('\r')) return from.replaceAll('\r\n', '\n');
  return from;
}

/** A timeout counts as CAUGHT only if the output shows a test that was running; an opaque hang is TIMEOUT. */
function classify(result, output) {
  if (result.error?.code === 'ETIMEDOUT') return /\ntest .+ \.\.\. /.test(output) ? 'CAUGHT' : 'TIMEOUT';
  // A mutation that does not compile (or any unexpected failure of the command) is a defect of the
  // mutation or the environment, not a catch: it must fail the run and be investigated.
  if (/error\[/.test(output) || /could not compile/.test(output)) return 'INVALID';
  if (result.status === 101 && /(test result: )?FAILED/m.test(output)) return 'CAUGHT';
  if (result.status === 0) return 'ESCAPED';
  return 'INVALID';
}

export function runMutation(mutation, deps) {
  const root = deps.root;
  const file = path.join(root, mutation.file);
  const original = readFileSync(file, 'utf8');
  const anchor = anchorFor(original, mutation.from);
  const at = original.indexOf(anchor);
  if (at < 0) return { outcome: 'ANCHOR_MISSING', seconds: 0 };
  if (original.indexOf(anchor, at + 1) >= 0) return { outcome: 'ANCHOR_AMBIGUOUS', seconds: 0 };
  let written = false;
  const started = Date.now();
  const seconds = () => Math.round((Date.now() - started) / 1000);
  try {
    // indexOf + slicing, never String.replace: replacement text is literal, $ patterns must survive.
    writeFileSync(file, original.slice(0, at) + mutation.to + original.slice(at + anchor.length));
    written = true;
    deps.guard?.set(file, original);
    const command = deps.env.MUTATE_COMMAND
      ? JSON.parse(deps.env.MUTATE_COMMAND)
      : ['node', 'scripts/cargo.mjs', 'test', '--locked', '-p', mutation.package,
        ...(mutation.features ? ['--features', mutation.features] : []),
        ...(mutation.test?.length ? ['--', ...mutation.test] : [])];
    let result;
    try {
      result = deps.spawn(command[0], command.slice(1), {
        cwd: root,
        shell: false,
        stdio: ['ignore', 'pipe', 'pipe'],
        timeout: Number(deps.env.MUTATE_TIMEOUT_MS) || DEFAULT_TIMEOUT_MS,
      });
    } catch (error) {
      return { outcome: 'INVALID', seconds: seconds(), detail: error.message };
    }
    const output = String(result.stdout ?? '') + String(result.stderr ?? '');
    const outcome = classify(result, output);
    return { outcome, seconds: seconds(), detail: outcome === 'INVALID' && result.error ? String(result.error) : undefined };
  } finally {
    deps.guard?.clear();
    if (written) writeFileSync(file, original);
  }
}

export function run(argv, deps = {}) {
  const env = deps.env ?? process.env;
  const root = deps.root ?? fileURLToPath(new URL('../../', import.meta.url));
  const spawn = deps.spawn ?? spawnSync;
  const log = deps.log ?? ((line) => console.log(line));
  const { list: listArgument, shard, filter } = parseArgv(argv);
  let regex = null;
  if (filter !== null) {
    try { regex = new RegExp(filter); } catch (error) { throw new Error(`--filter is not a valid regular expression: ${error.message}`, { cause: error }); }
  }
  const shardParts = shard === null ? null : parseShard(shard);
  let mutations = loadList(listArgument, root);
  if (regex) mutations = mutations.filter(mutation => regex.test(mutation.name));
  if (shardParts) mutations = selectShard(mutations, shardParts.i, shardParts.n);

  let current = null;
  const guard = {
    set: (file, text) => { current = { file, text }; },
    clear: () => { current = null; },
  };
  const onSignal = (code) => () => {
    if (current) { writeFileSync(current.file, current.text); current = null; }
    process.exit(code);
  };
  const sigint = onSignal(130, 'SIGINT');
  const sigterm = onSignal(143, 'SIGTERM');
  process.once('SIGINT', sigint);
  process.once('SIGTERM', sigterm);
  const summary = env.GITHUB_STEP_SUMMARY;
  const tally = Object.fromEntries(OUTCOMES.map(outcome => [outcome, 0]));
  const rows = [];
  try {
    for (const mutation of mutations) {
      const result = runMutation(mutation, { ...deps, env, root, spawn, guard });
      tally[result.outcome] += 1;
      rows.push([result.outcome, mutation.name, result.seconds, result.detail]);
      log(`${result.outcome} ${mutation.name} (${result.seconds} s)${result.detail ? ` — ${result.detail}` : ''}`);
    }
    log(OUTCOMES.map(outcome => `${outcome}: ${tally[outcome]}`).join(', '));
    if (summary) {
      const table = ['| Outcome | Mutation | Seconds |', '| --- | --- | --- |',
        ...rows.map(([outcome, name, seconds]) => `| ${outcome} | ${name} | ${seconds} |`),
        `| TOTALS | | ${rows.reduce((sum, row) => sum + row[2], 0)} |`];
      appendFileSync(summary, `${table.join('\n')}\n`);
    }
  } finally {
    if (current) writeFileSync(current.file, current.text);
    process.removeListener('SIGINT', sigint);
    process.removeListener('SIGTERM', sigterm);
  }
  const code = rows.every(row => row[0] === 'CAUGHT') ? 0 : 1;
  return { code, tally, rows };
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  process.exitCode = run(process.argv.slice(2)).code;
}
