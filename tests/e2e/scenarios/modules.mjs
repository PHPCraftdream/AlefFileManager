// SPDX-License-Identifier: MIT OR Apache-2.0
// Scenarios of the framework modules (docs/stages/m2-desktop.md, "Приёмка"): `app` (+ `quit`, `relaunch`,
// the generated usage text) and the system modules `path` and `os`.
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync } from 'node:fs';
import os from 'node:os';
import { join } from 'node:path';

import { prepareSite, scratch, startApp, verdictOf } from '../lib.mjs';

const APP_CHECKS = [
  'app-info-matches-the-manifest', 'app-args-are-parsed-by-the-manifest-schema',
  'app-env-listed-variable-is-readable', 'app-env-unlisted-variable-is-denied',
  'app-env-without-a-name-lists-only-the-listed-variables',
  'app-cwd-is-the-working-directory-of-the-process', 'app-quit-rejects-a-code-outside-0-255',
];

const SYSTEM_CHECKS = [
  'path-directories-are-absolute-and-the-app-ones-end-with-the-id', 'path-join-normalize-dirname-basename',
  'os-info-describes-this-machine', 'os-theme-is-light-or-dark', 'os-theme-changed-subscription-can-be-made-and-undone',
];

const sameFile = (left, right) => {
  const [a, b] = [left, right].map(path => realpathSync(path));
  return process.platform === 'win32' ? a.toLowerCase() === b.toLowerCase() : a === b;
};

/** What the runtime must do with a command line, judged before any window opens. */
const USAGE_CASES = [
  { name: 'help lists the options of the manifest', tail: ['--help'], exit: 0,
    mentions: ['Usage: Alef e2e app [OPTIONS] [files...]', '-p, --port <number>', 'Port to listen on', '--label <text>', 'Files to open', '-V, --version'] },
  { name: 'short help is the same', tail: ['-h'], exit: 0, mentions: ['Usage: Alef e2e app', '-h, --help'] },
  { name: 'version prints name and version', tail: ['--version'], exit: 0, mentions: ['Alef e2e app 4.5.6'] },
  { name: 'an unknown option is a usage error', tail: ['--bogus'], exit: 2, mentions: ['unknown option --bogus'] },
  { name: 'a number option refuses text', tail: ['--port', 'abc'], exit: 2, mentions: ['--port expects a number'] },
  { name: 'an option without its value is a usage error', tail: ['--port'], exit: 2, mentions: ['--port requires a value'] },
  { name: 'an application without a schema takes no arguments', app: 'core', tail: ['stray'], exit: 2, mentions: ['unexpected argument "stray"'] },
  { name: 'help works without a schema', app: 'core', tail: ['--help'], exit: 0, mentions: ['Usage: Alef e2e core [OPTIONS]', '-V, --version'] },
];

export function moduleScenarios({ drive, exe, verbose }) {
  return {
    app: () => drive({
      name: 'app', app: 'modules/app', targets: { cwd: process.cwd() },
      env: { ALEF_E2E_ENV_ALLOWED: 'yes', ALEF_E2E_ENV_SECRET: 'secret-value-31337' },
      args: ['--', '-p', '8080', '--verbose', '--label=check', 'y.txt'],
      expectedChecks: APP_CHECKS,
    }),

    // The exit code of the process is the code the page passed to `app.quit`.
    quit: () => drive({
      name: 'quit', app: 'modules/app', targets: { cwd: process.cwd() },
      env: { ALEF_E2E_ENV_ALLOWED: 'yes', ALEF_E2E_ENV_SECRET: 'secret-value-31337' },
      args: ['--', '-p', '8080', '--verbose', '--label=quit', 'y.txt'],
      expectedChecks: APP_CHECKS,
      judge: async (_lines, _result, running) => {
        const exit = await running.waitForExit(20000);
        return exit?.code === 7 ? [] : [`app.quit(7) ended the process with ${exit ? `code ${exit.code}` : 'no exit within 20 s'}`];
      },
    }),

    // The first instance restarts the program; the second one shares the log pipe and reports.
    async relaunch() {
      const directory = mkdtempSync(join(scratch, 'marker-'));
      const site = prepareSite('relaunch', 'modules/relaunch');
      const running = startApp({
        exe, args: ['--app', site, '--', '--label=relaunch'], env: { ALEF_E2E_MARKER: join(directory, 'once') }, verbose,
      });
      const problems = [];
      try {
        await running.waitFor(line => line.includes('ALEF_E2E RESULT'), 120000, 'the verdict of the second instance', { survivesExit: true });
        const result = verdictOf(running.lines);
        if (result?.[1] !== 'PASS') problems.push(`verdict ${result?.[1]}: ${result?.[2]}`);
        if (!running.lines.some(line => line.includes('ALEF_E2E relaunch first-instance'))) problems.push('the first instance never reported');
        if (!running.lines.some(line => / check relaunch-starts-a-second-instance-with-the-same-arguments ok /.test(line))) problems.push('the second instance did not confirm its arguments');
        const pids = running.lines.filter(line => line.startsWith('ALEF_READY')).map(line => /pid=(\d+)/.exec(line)?.[1]);
        if (pids.length !== 2 || pids[0] === pids[1]) problems.push(`expected two different instances, saw ${JSON.stringify(pids)}`);
        const first = await running.waitForExit(15000);
        if (first?.code !== 0) problems.push(`the first instance exited with ${first ? `code ${first.code}` : 'no exit'}`);
        if (!(await running.waitForClosed(30000))) problems.push('the second instance did not quit by itself');
      } catch (error) {
        problems.push(error.message);
      } finally {
        running.stop();
        rmSync(directory, { recursive: true, force: true });
      }
      return { problems, lines: running.lines };
    },

    system: () => drive({
      name: 'system', app: 'modules/system',
      targets: { platform: process.platform, arch: process.arch, hostname: os.hostname() },
      expectedChecks: SYSTEM_CHECKS,
      judge: lines => {
        const reported = Object.fromEntries(lines
          .map(line => /^ALEF_E2E path (\w+) (.+)$/.exec(line))
          .filter(Boolean)
          .map(([, name, value]) => [name, value]));
        const problems = [];
        for (const name of ['temp', 'home', 'executable']) {
          if (!reported[name] || !existsSync(reported[name])) problems.push(`path.${name} does not exist: ${reported[name]}`);
        }
        if (reported.executable && existsSync(reported.executable) && !sameFile(reported.executable, exe)) {
          problems.push(`path.executable ${reported.executable} is not the binary that was started (${exe})`);
        }
        return problems;
      },
    }),

    // `--help`, `--version` and usage errors: decided from the manifest schema before a window opens.
    async arguments() {
      const problems = [];
      mkdirSync(scratch, { recursive: true });
      for (const item of USAGE_CASES) {
        const site = prepareSite(`usage-${USAGE_CASES.indexOf(item)}`, item.app ?? 'modules/app');
        const running = startApp({ exe, args: ['--app', site, '--', ...item.tail], verbose });
        const exit = await running.waitForExit(30000);
        const log = running.lines.join('\n');
        running.stop();
        const wrong = [];
        if (exit?.code !== item.exit) wrong.push(`exit code ${exit?.code}, expected ${item.exit}`);
        for (const needle of item.mentions) if (!log.includes(needle)) wrong.push(`the output does not mention ${JSON.stringify(needle)}`);
        if (log.includes('ALEF_READY')) wrong.push('the runtime started although the command line asked for no run');
        console.log(`    ${wrong.length === 0 ? 'ok  ' : 'FAIL'} ${item.name}${wrong.length ? `: ${wrong.join('; ')}\n         log: ${log.slice(0, 300)}` : ''}`);
        problems.push(...wrong.map(text => `${item.name}: ${text}`));
      }
      return { problems, lines: [] };
    },
  };
}
