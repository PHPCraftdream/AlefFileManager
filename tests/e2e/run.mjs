// SPDX-License-Identifier: MIT OR Apache-2.0
// End-to-end runner: starts the generic `alef` runtime on scenario applications (tests/e2e/apps) and
// judges what the pages report through `e2e.report` and what the runtime and local servers saw.
//   node tests/e2e/run.mjs [--exe <alef binary>] [--only core,induced,permissions,csp,dev,manifest,app,quit,relaunch,instance,restore,system,window,desktop,consent,narrowing,ask,cancel,decisions,arguments,startup]
//                          [--timeout-s 150] [--verbose]
// Exit code 0 = every selected scenario passed.
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import { countingServer, exeName, field, interesting, makeDriver, root, scratch, staticServer, startApp } from './lib.mjs';
import { consentScenarios } from './scenarios/consent.mjs';
import { manifestCases } from './scenarios/manifests.mjs';
import { moduleScenarios } from './scenarios/modules.mjs';
import { startupScenarios } from './scenarios/startup.mjs';

const args = process.argv.slice(2);
const option = name => { const at = args.indexOf(name); return at < 0 ? undefined : args[at + 1]; };
const verbose = args.includes('--verbose');
const timeoutMs = Number(option('--timeout-s') ?? 150) * 1000;
const exe = option('--exe') ?? join(root, 'backend', 'target', 'debug', `alef${exeName}`);
const only = option('--only')?.split(',');
const MAX_ABORT_MS = 250;
const WINDOW_BYTES = 1024 * 1024;

const drive = makeDriver({ exe, verbose, timeoutMs });

const CORE_CHECKS = [
  'window-is-shown-once-it-has-content', 'hello', 'denials', 'json-echo', 'binary-echo-16MiB', 'unknown-command-is-not-found', 'legacy-invoke-route-is-gone',
  'credit-window', 'abort-closes-the-source', 'events-stream',
  'lib-connect', 'lib-call-json', 'lib-binary-roundtrip-4MiB', 'lib-error-mapping', 'lib-readable-acks-by-itself',
  'lib-close-stops-the-source', 'lib-events', 'lib-window-watch',
  ...['navigation', 'reload'].flatMap(how => [`${how}-new-token`, `${how}-old-token-denied`, `${how}-library-reconnects`]),
];

function judgeCore(lines) {
  const problems = [];
  const stopped = new Map(lines.filter(line => line.includes('producer-stopped')).map(line => [field(line, 'stream'), field(line, 'at_ms')]));
  const aborts = lines.filter(line => line.includes('abort-called'));
  if (aborts.length < 2) problems.push(`expected an abort from the raw and from the library check, saw ${aborts.length}`);
  for (const line of aborts) {
    const stream = field(line, 'stream');
    const delta = stopped.get(stream) - field(line, 'at_ms');
    console.log(`    abort of stream ${stream}: source closed in ${delta} ms (limit ${MAX_ABORT_MS} ms, target 100 ms)`);
    if (!(delta >= 0 && delta <= MAX_ABORT_MS)) problems.push(`stream ${stream}: source stopped after ${delta} ms`);
  }
  const floods = lines.filter(line => line.includes('flood-done'));
  if (floods.length < 2) problems.push(`expected two completed floods, saw ${floods.length}`);
  for (const line of floods) {
    const peak = field(line, 'peak_outstanding');
    console.log(`    ${line}`);
    if (!(peak > 0 && peak <= WINDOW_BYTES)) problems.push(`credit window exceeded: ${line}`);
  }
  // A stream left open by the first document must be closed by the runtime when it navigates away.
  const left = lines.findIndex(line => line.includes('stream-left-open'));
  if (left < 0) problems.push('the page did not report an abandoned stream');
  else {
    const stream = field(lines[left], 'stream');
    const closed = lines.findIndex((line, index) => index > left && line.includes('producer-stopped') && field(line, 'stream') === stream);
    console.log(`    abandoned stream ${stream}: ${closed < 0 ? 'NOT closed' : 'source closed after the document went away'}`);
    if (closed < 0) problems.push(`the runtime did not close the abandoned stream ${stream} when the document was replaced`);
  }
  return problems;
}

const INDUCED = ['binary-echo-16MiB', 'lib-binary-roundtrip-4MiB'];

const scenarios = {
  ...moduleScenarios({ drive, exe, verbose }),
  ...consentScenarios({ drive, exe, verbose }),
  ...startupScenarios({ exe }),
  core: () => drive({ name: 'core', app: 'core', expectedChecks: CORE_CHECKS, judge: judgeCore }),

  // The echo is corrupted on purpose: the runner must see exactly the two binary checks fail.
  induced: () => drive({
    name: 'induced', app: 'core', env: { ALEF_E2E_BREAK: '1' }, expectedChecks: [],
    judge: (_lines, result) => {
      if (result?.[1] !== 'FAIL') return ['the induced failure was not detected'];
      const failed = result[2].split(',').filter(Boolean).sort();
      return JSON.stringify(failed) === JSON.stringify([...INDUCED].sort()) ? [] : [`expected exactly ${INDUCED} to fail, got ${failed}`];
    },
  }),

  permissions: () => drive({
    name: 'permissions', app: 'permissions', env: { ALEF_E2E_ALLOWED: 'yes', ALEF_E2E_SECRET: 'no' },
    expectedChecks: [
      'fs-read-inside-the-scope-is-allowed', 'fs-read-outside-the-scope-is-denied',
      'fs-write-without-the-permission-is-denied', 'app-env-only-the-listed-variable-is-allowed',
      'a-denied-call-does-not-poison-the-session',
    ],
  }),

  async csp() {
    const problems = [];
    for (const [mode, expectHits] of [['closed', 0], ['open', 1]]) {
      const server = await countingServer();
      try {
        const connect = mode === 'open' ? `[ ${server.origin} ]` : '[]';
        const result = await drive({
          name: `csp-${mode}`, app: 'csp', replacements: { CONNECT: connect }, targets: { origin: server.origin, expect: mode },
          expectedChecks: [`external-fetch-is-${mode === 'open' ? 'allowed' : 'blocked'}`, 'inline-script-is-blocked', 'the-transport-still-works-under-the-policy'],
        });
        problems.push(...result.problems.map(problem => `${mode}: ${problem}`));
        const arrived = server.hits.filter(hit => hit.startsWith('GET /ping')).length;
        console.log(`    ${mode}: ${arrived} request(s) reached the external server`);
        if (mode === 'closed' && arrived !== 0) problems.push('closed: a request reached the external server despite an empty external.connect');
        if (mode === 'open' && arrived < expectHits) problems.push('open: the listed origin was never contacted');
      } finally {
        await server.close();
      }
    }
    return { problems, lines: [] };
  },

  // `--dev-url`: the document is served by a local HTTP server instead of the application files.
  async dev() {
    const directory = join(scratch, 'sites', 'dev');
    const server = await staticServer(directory);
    const probe = await staticServer(directory); // a second origin: it must not get a session
    try {
      return await drive({
        name: 'dev', app: 'dev', targets: { probeOrigin: probe.origin },
        appArgs: ['--app', directory, '--dev-url', `${server.origin}/`],
        expectedChecks: [
          'the-page-comes-from-the-development-server', 'lib-connect-and-call-from-the-http-origin',
          'binary-roundtrip-from-the-http-origin', 'events-reach-the-http-origin',
          'a-foreign-http-origin-holding-the-capability-is-refused',
        ],
        judge: () => [
          ...(server.requests.some(request => request.startsWith('GET /index.html')) ? [] : ['the page was never requested from the development server']),
          ...(probe.requests.some(request => request.startsWith('GET /probe.html')) ? [] : ['the foreign-origin probe was never loaded']),
        ],
      });
    } finally {
      await server.close();
      await probe.close();
    }
  },

  // Manifests the runtime must refuse before opening a window, each with a message that names the cause.
  async manifest() {
    const problems = [];
    for (const item of manifestCases()) {
      const directory = mkdtempSync(join(scratch, 'manifest-'));
      if (item.manifest !== null) writeFileSync(join(directory, 'alef.ktav'), item.manifest);
      const running = startApp({ exe, args: item.args?.(directory) ?? ['--app', directory], verbose });
      const exit = await running.waitForExit(30000);
      const log = running.lines.join('\n');
      rmSync(directory, { recursive: true, force: true });
      const wrong = [];
      if (exit?.code !== item.exitCode) wrong.push(`exit code ${exit?.code}, expected ${item.exitCode}`);
      for (const needle of item.mentions) if (!log.includes(needle)) wrong.push(`the message does not mention "${needle}"`);
      if (log.includes('ALEF_READY')) wrong.push('the runtime started although it had to refuse');
      console.log(`    ${wrong.length === 0 ? 'ok  ' : 'FAIL'} ${item.name}${wrong.length ? `: ${wrong.join('; ')}\n         log: ${log.slice(0, 300)}` : ''}`);
      problems.push(...wrong.map(text => `${item.name}: ${text}`));
    }
    return { problems, lines: [] };
  },
};

mkdirSync(scratch, { recursive: true });
let failures = 0;
const selected = Object.keys(scenarios).filter(name => !only || only.includes(name));
if (only && selected.length !== only.length) {
  console.error(`unknown scenario in --only; known: ${Object.keys(scenarios)}`);
  process.exit(2);
}
for (const name of selected) {
  console.log(`== ${name}`);
  let outcome;
  try {
    outcome = await scenarios[name]();
  } catch (error) {
    outcome = { problems: [error.message], lines: [] };
  }
  for (const problem of outcome.problems) console.log(`    PROBLEM ${problem}`);
  if (outcome.problems.length > 0) {
    failures += 1;
    for (const line of interesting(outcome.lines).slice(0, 12)) console.log(`    log: ${line}`);
  }
  console.log(`== ${name}: ${outcome.problems.length === 0 ? 'PASS' : 'FAIL'}`);
}
rmSync(scratch, { recursive: true, force: true });
console.log(failures === 0 ? 'e2e: all scenarios passed' : `e2e: ${failures} scenario(s) failed`);
process.exitCode = failures === 0 ? 0 : 1;
