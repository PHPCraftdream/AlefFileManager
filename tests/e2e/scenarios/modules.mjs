// SPDX-License-Identifier: MIT OR Apache-2.0
// Scenarios of the framework modules (docs/stages/m2-desktop.md, "Приёмка"): `app` (+ `quit`, `relaunch`,
// the generated usage text, `instance`), the system modules `path` and `os`, `window` and `desktop` (`dialog`, `shell`, `clipboard`).
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import { join } from 'node:path';

import { prepareSite, root, scratch, startApp, verdictOf } from '../lib.mjs';

const APP_CHECKS = [
  'app-info-matches-the-manifest', 'app-args-are-parsed-by-the-manifest-schema',
  'app-env-listed-variable-is-readable', 'app-env-unlisted-variable-is-denied',
  'app-env-without-a-name-lists-only-the-listed-variables',
  'app-cwd-is-the-working-directory-of-the-process', 'app-quit-rejects-a-code-outside-0-255',
];

const CONSOLE_CHECKS = [
  'console-stdin-comes-whole-and-goes-out-changed-on-stdout', 'console-stderr-carries-a-line-of-its-own',
  'console-the-windows-and-the-dialogs-are-not-available', 'console-stdin-is-taken-once',
];

const SERVICE_CHECKS = [
  'service-serves-without-a-window', 'service-is-asked-before-it-ends-on-a-signal',
  'service-has-no-console-and-no-window',
];

/** A process without a window needs OpenGL for its hidden WebView; a machine with no driver (a runner of Windows) has none. */
const withoutOpenGl = lines => process.platform === 'win32' && lines.some(line => line.includes('no software rendering context'));

/** A port nothing listens on. */
async function freePort() {
  const server = net.createServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  await new Promise(resolve => server.close(resolve));
  return port;
}

/** Asks the service on `port` for `path` until it answers (a few seconds at most). */
async function ask(port, path) {
  const deadline = Date.now() + 15000;
  for (;;) {
    try {
      const answer = await fetch(`http://127.0.0.1:${port}${path}`);
      return { status: answer.status, text: await answer.text() };
    } catch (error) {
      if (Date.now() > deadline) throw error;
      await new Promise(resolve => setTimeout(resolve, 100));
    }
  }
}

const SYSTEM_CHECKS = [
  'path-directories-are-absolute-and-the-app-ones-end-with-the-id', 'path-join-normalize-dirname-basename',
  'os-info-describes-this-machine', 'os-theme-is-light-or-dark', 'os-theme-changed-subscription-can-be-made-and-undone',
  'screen-agrees-with-the-state-of-the-window', 'window-create-is-denied-without-the-permission',
  'clipboard-read-and-shell-are-denied-without-the-rights',
];

const DESKTOP_CHECKS = [
  'dialog-options-are-refused-before-a-dialog-is-shown', 'dialog-open-returns-the-chosen-files',
  'dialog-open-cancelled-is-an-empty-list', 'dialog-open-folder-returns-the-folder',
  'dialog-save-returns-the-path-and-null-when-cancelled', 'dialog-message-and-confirm-answer-plainly',
  'dialog-answer-of-the-wrong-kind-is-an-error', 'a-dialog-without-a-scripted-answer-fails-instead-of-waiting',
  'shell-open-external-inside-the-scope-is-opened', 'shell-open-external-outside-the-scope-is-denied',
  'shell-paths-in-the-scope-are-opened-shown-and-trashed', 'shell-paths-outside-the-scope-are-denied',
  'shell-open-path-does-not-start-a-program', 'shell-a-path-that-is-not-there-is-not-found',
  'clipboard-text-passes-whatever-its-size', 'clipboard-html-passes-and-text-replaces-it',
  'clipboard-image-passes-as-png-and-text-replaces-it', 'clipboard-refuses-what-is-not-an-image-or-not-text',
  'notification-is-shown-and-its-text-and-icon-are-checked',
];

/** The same file whatever the spelling: links resolved, and the case of Windows ignored. */
const real = path => {
  const resolved = realpathSync.native(path);
  return process.platform === 'win32' ? resolved.toLowerCase() : resolved;
};

const WINDOW_CHECKS = [
  'declared-windows-are-open', 'screen-describes-the-displays', 'size-in-percent-of-the-work-area',
  'the-window-is-centred-in-the-work-area', 'the-size-and-position-of-a-declared-window',
  'create-opens-a-window-with-percent-size-and-an-explicit-position', 'set-size-and-set-position-accept-percentages',
  'minimum-and-maximum-size-are-enforced', 'maximize-and-restore-change-the-state',
  'events-moved-and-resized-name-their-window', 'title-zoom-hide-and-show', 'label-and-url-rules-are-enforced',
  'close-requested-can-be-prevented', 'a-document-can-refuse-and-then-allow-the-close-of-its-window',
  'files-dropped-on-a-window-are-announced-and-become-readable',
  'an-unanswered-close-request-closes-the-window-after-the-limit', 'destroy-does-not-wait-for-the-document',
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

const CLI_CHECKS = [
  'cli-exec-runs-a-program-and-reports-its-code-and-output', 'cli-exec-feeds-stdin-and-captures-stderr',
  'cli-exec-through-a-shell', 'cli-exec-times-out-and-kills-the-tree', 'cli-spawn-pipes-streams-both-ways',
  'cli-kill-takes-down-the-grandchild', 'cli-rights-are-held', 'cli-exec-resolution-ignores-the-page-env',
  'cli-pty-is-interactive-and-resizes', 'cli-pty-kill-takes-down-the-grandchild',
  'cli-pty-validates-dimensions-and-rights',
];
const CLI_SUBSTITUTE_CHECKS = [
  'cli-exec-substituted-hangs-and-times-out-quickly',
  'cli-pty-substituted-stays-pending-without-starting-and-aborts',
];
const CLI_COMMAND_CHECKS = [
  'cli-run-declared-params-and-body', 'cli-start-declared-params-and-streams',
  'cli-sidecar-declared-and-direct-launch', 'cli-declared-command-rights-are-separate',
  'cli-declared-substituted-times-out',
];

export function moduleScenarios({ drive, exe, verbose }) {
  async function service({ signal }) {
    const port = await freePort();
    const site = prepareSite(signal ? 'service-signal' : 'service', 'modules/app/service', { replacements: { PORT: String(port) }, targets: { port } });
    const running = startApp({ exe, args: ['--app', site], verbose });
    const problems = [];
    try {
      try {
        await running.waitFor(line => line.includes('ALEF_E2E RESULT'), 120000, 'the verdict of the page');
      } catch (error) {
        if (withoutOpenGl(running.lines)) {
          console.log('    skipped: this machine has no OpenGL driver for a process without a window');
          return { problems: [], lines: running.lines, site };
        }
        throw error;
      }
      const result = verdictOf(running.lines);
      if (result?.[1] !== 'PASS') problems.push(`verdict ${result?.[1]}: ${result?.[2]}`);
      const done = new Set(running.lines.map(line => / check (\S+) ok /.exec(line)?.[1]));
      const absent = SERVICE_CHECKS.filter(name => !done.has(name));
      if (absent.length > 0) problems.push(`checks without an ok line: ${absent}`);
      const pong = await ask(port, '/ping');
      if (pong.status !== 200 || pong.text !== 'pong') problems.push(`/ping answered ${pong.status} ${pong.text}`);
      if (!running.lines.some(line => line === 'ALEF_MODE windowless console=false')) problems.push('the runtime did not say it runs without a window');
      if (signal) {
        process.kill(running.pid, 'SIGTERM');
      } else {
        const bye = await ask(port, '/quit');
        if (bye.text !== 'bye') problems.push(`/quit answered ${bye.text}`);
      }
      const exit = await running.waitForExit(30000);
      const expected = signal ? 143 : 5;
      if (exit?.code !== expected) problems.push(`the exit code is ${exit ? exit.code : 'none'}, expected ${expected}`);
      const heard = running.lines.some(line => / check service-heard-before-quit ok /.test(line));
      // `app.exit` is `app.quit`: the page is asked either way.
      if (!heard) problems.push(`the page was not asked before ${signal ? 'the signal' : 'its own exit'} ended the process`);
    } catch (error) {
      problems.push(error.message);
    } finally {
      running.stop();
    }
    return { problems, lines: running.lines, site };
  }

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

    // Two declared windows, a created one, geometry, events, the close request; the process ends with its last window.
    // The files of the drop check are made here: they must lie outside every scope of the manifest.
    async window() {
      const directory = mkdtempSync(join(scratch, 'drop-'));
      const folder = join(directory, 'folder');
      mkdirSync(join(folder, 'deep'), { recursive: true });
      for (const path of [join(directory, 'a.txt'), join(directory, 'b.txt'), join(folder, 'deep', 'c.txt')]) writeFileSync(path, 'x');
      try {
        return await drive({
          name: 'window', app: 'modules/window',
          targets: {
            drop: {
              file: join(directory, 'a.txt'), folder, inside: join(folder, 'deep', 'c.txt'),
              sibling: join(directory, 'b.txt'), missing: join(directory, 'never-was.txt'),
            },
          },
          expectedChecks: WINDOW_CHECKS,
          judge: async (_lines, _result, running) => {
            const exit = await running.waitForExit(30000);
            return exit?.code === 0 ? [] : [`closing the last window ended the process with ${exit ? `code ${exit.code}` : 'no exit within 30 s'}`];
          },
        });
      } finally {
        rmSync(directory, { recursive: true, force: true });
      }
    },

    // The first instance restarts the program; the second one shares the log pipe and reports.
    async relaunch() {
      const directory = mkdtempSync(join(scratch, 'marker-'));
      const site = prepareSite('relaunch', 'modules/lifecycle/relaunch');
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

    // Two processes of one application: the first holds the endpoint and hears of the second, which learns
    // that it is not the first and quits; the first vetoes a quit, then allows one and exits with code 9.
    async instance() {
      const site = prepareSite('instance', 'modules/lifecycle/instance', { targets: { cwd: process.cwd() } });
      const first = startApp({ exe, args: ['--app', site, '--', '--role=first'], verbose });
      let second;
      const problems = [];
      const checked = (lines, names) => names.filter(name => !lines.some(line => new RegExp(` check ${name} ok `).test(line)));
      const exitOf = async (running, ms) => (await running.waitForExit(ms))?.code;
      try {
        await first.waitFor(line => line.includes('ALEF_E2E instance first-ready'), 120000, 'the first instance to hold the endpoint');
        second = startApp({ exe, args: ['--app', site, '--', '--role=second', 'one.txt', 'two.txt'], verbose });
        await second.waitFor(line => line.includes('ALEF_E2E RESULT'), 120000, 'the verdict of the second instance', { survivesExit: true });
        if (verdictOf(second.lines)?.[1] !== 'PASS') problems.push(`the second instance: ${verdictOf(second.lines)?.[2]}`);
        const absent = checked(second.lines, ['a-later-instance-is-told-so']);
        if (absent.length > 0) problems.push(`the second instance has no ok line for ${absent}`);
        const secondExit = await exitOf(second, 30000);
        if (secondExit !== 0) problems.push(`the second instance exited with ${secondExit ?? 'no exit'}`);
        await first.waitFor(line => line.includes('ALEF_E2E RESULT'), 120000, 'the verdict of the first instance', { survivesExit: true });
        if (verdictOf(first.lines)?.[1] !== 'PASS') problems.push(`the first instance: ${verdictOf(first.lines)?.[2]}`);
        const missing = checked(first.lines, ['the-first-instance-is-told-so', 'a-later-instance-is-announced-with-its-arguments-and-directory', 'before-quit-can-be-vetoed-and-then-allowed']);
        if (missing.length > 0) problems.push(`the first instance has no ok line for ${missing}`);
        const firstExit = await exitOf(first, 30000);
        if (firstExit !== 9) problems.push(`the first instance ended with ${firstExit ?? 'no exit'} instead of the code 9 of the allowed quit`);
      } catch (error) {
        problems.push(error.message);
      } finally {
        first.stop();
        second?.stop();
        // The application cache of this test application holds the endpoint; nothing else of it is kept.
        const cache = first.lines.map(line => /ALEF_E2E path appCache (.+)$/.exec(line)?.[1]).find(Boolean);
        if (cache && /org\.alef\.e2e\.modules\.instance$/.test(cache)) rmSync(cache, { recursive: true, force: true });
      }
      return { problems, lines: [...first.lines, ...(second?.lines ?? [])] };
    },

    // A window with `restore: true` opens where it was left: five runs of one application, and between them
    // the runner reads the file the runtime wrote and rewrites it (a place on no display, a damaged file).
    async restore() {
      const tolerance = process.platform === 'linux' ? 80 : 3;
      const site = prepareSite('restore', 'modules/lifecycle/restore', { targets: { tolerance } });
      const problems = [];
      let appData;
      const run = async step => {
        const running = startApp({ exe, args: ['--app', site, '--', `--step=${step}`], verbose });
        try {
          await running.waitFor(line => line.includes('ALEF_E2E RESULT'), 120000, `the verdict of ${step}`, { survivesExit: true });
          if (verdictOf(running.lines)?.[1] !== 'PASS') problems.push(`${step}: ${verdictOf(running.lines)?.[2]}`);
          const exit = await running.waitForExit(30000);
          if (exit?.code !== 0) problems.push(`${step}: the run ended with ${exit?.code ?? 'no exit'}`);
        } catch (error) {
          problems.push(`${step}: ${error.message}`);
        } finally {
          running.stop();
        }
        appData ??= running.lines.map(line => /ALEF_E2E path appData (.+)$/.exec(line)?.[1]).find(Boolean);
        return running.lines;
      };
      try {
        await run('save');
        const stateFile = appData && join(appData, 'window-state.json');
        if (!stateFile || !existsSync(stateFile)) {
          problems.push('the first run left no window-state.json');
        } else {
          const saved = JSON.parse(readFileSync(stateFile, 'utf8'));
          const main = saved.windows?.main;
          if (saved.version !== 1 || !main || Math.abs(main.width - 777) > tolerance || Math.abs(main.height - 555) > tolerance || main.maximized !== false) {
            problems.push(`the file holds ${JSON.stringify(saved)}`);
          }
          await run('reopen');
          const afterMaximize = JSON.parse(readFileSync(stateFile, 'utf8')).windows?.main;
          if (afterMaximize?.maximized !== true || Math.abs(afterMaximize.width - 777) > tolerance) {
            problems.push(`a maximized window must be remembered as maximized with the size it had: ${JSON.stringify(afterMaximize)}`);
          }
          await run('maximized');
          writeFileSync(stateFile, JSON.stringify({ version: 1, windows: { main: { x: 99999, y: 99999, width: 700, height: 500, maximized: false } } }));
          await run('gone');
          writeFileSync(stateFile, 'not json');
          await run('damaged');
        }
      } catch (error) {
        problems.push(error.message);
      } finally {
        // The data folder of this test application holds the state file and nothing else.
        if (appData && /org\.alef\.e2e\.modules\.restore$/.test(appData)) rmSync(appData, { recursive: true, force: true });
      }
      return { problems, lines: [] };
    },

    // Dialogs answered from a script, a shell that only logs, a clipboard in memory: the run shows nothing
    // and touches nothing of the user, and the runner reads what the shell was asked.
    async desktop() {
      const directory = mkdtempSync(join(os.tmpdir(), 'alef-e2e-desktop-'));
      const at = name => join(directory, name);
      const folder = at('picked');
      mkdirSync(folder);
      for (const name of ['chosen.txt', 'other.txt', 'note.txt', 'old.txt', 'setup.exe', 'icon.png']) writeFileSync(at(name), name);
      const log = at('shell.log');
      const notifications = at('notifications.log');
      const script = [
        { open: [at('chosen.txt')] }, { open: [at('chosen.txt'), at('other.txt')] }, { open: [] }, { open: [folder] },
        { save: at('report.txt') }, { save: null }, { message: null }, { confirm: true }, { confirm: false },
        { save: at('report.txt') }, // the page asks for `open` here: a script of the wrong kind is an error
      ];
      try {
        return await drive({
          name: 'desktop', app: 'modules/desktop',
          targets: {
            directory, chosen: at('chosen.txt'), other: at('other.txt'), folder, saved: at('report.txt'),
            note: at('note.txt'), old: at('old.txt'), tool: at('setup.exe'), gone: at('never-was.txt'),
            outside: join(root, 'package.json'), icon: at('icon.png'),
          },
          env: { ALEF_E2E_DIALOGS: JSON.stringify(script), ALEF_E2E_SHELL_LOG: log, ALEF_E2E_NOTIFICATION_LOG: notifications },
          expectedChecks: DESKTOP_CHECKS,
          judge: () => {
            const asked = existsSync(log)
              ? readFileSync(log, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line))
              : [];
            const expected = [
              ['openExternal', 'https://example.com/docs/guide?page=2'],
              ['openPath', real(at('note.txt'))], ['showInFolder', real(at('note.txt'))], ['trash', real(at('old.txt'))],
            ];
            const seen = asked.map(({ operation, target }) => [operation, operation === 'openExternal' ? target : real(target)]);
            const problems = JSON.stringify(seen) === JSON.stringify(expected)
              ? []
              : [`the shell was asked ${JSON.stringify(seen)}, expected exactly ${JSON.stringify(expected)}`];
            const shown = existsSync(notifications)
              ? readFileSync(notifications, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line))
              : [];
            const wanted = [
              { title: 'Done', body: 'All saved\nin two places', icon: null },
              { title: 'With an icon', body: '', icon: real(at('icon.png')) },
            ];
            const got = shown.map(({ title, body, icon }) => ({ title, body, icon: icon === null ? null : real(icon) }));
            if (JSON.stringify(got) !== JSON.stringify(wanted)) {
              problems.push(`the desktop was asked to show ${JSON.stringify(got)}, expected exactly ${JSON.stringify(wanted)}`);
            }
            return problems;
          },
        });
      } finally {
        rmSync(directory, { recursive: true, force: true });
      }
    },

    // A console utility: no window, the input on stdin comes back changed on stdout, the exit code is the page's.
    async console() {
      const size = 3 * 1024 * 1024 + 17;
      const input = Buffer.alloc(size);
      for (let at = 0; at < size; at += 1) input[at] = (at * 31 + (at >> 8)) & 255;
      const site = prepareSite('console', 'modules/app/console', { targets: { size } });
      const running = startApp({ exe, args: ['--app', site], input, verbose });
      const problems = [];
      try {
        try {
          await running.waitFor(line => line.includes('ALEF_E2E RESULT'), 120000, 'the verdict of the page');
        } catch (error) {
          if (withoutOpenGl(running.lines)) {
            console.log('    skipped: this machine has no OpenGL driver for a process without a window');
            return { problems: [], lines: running.lines, site };
          }
          throw error;
        }
        const result = verdictOf(running.lines);
        if (result?.[1] !== 'PASS') problems.push(`verdict ${result?.[1]}: ${result?.[2]}`);
        const done = new Set(running.lines.map(line => / check (\S+) ok /.exec(line)?.[1]));
        const absent = CONSOLE_CHECKS.filter(name => !done.has(name));
        if (absent.length > 0) problems.push(`checks without an ok line: ${absent}`);
        const exit = await running.waitForExit(30000);
        if (exit?.code !== 7) problems.push(`the exit code is ${exit ? exit.code : 'none'}, expected 7`);
        const out = running.stdout();
        const expected = Buffer.from(input.map(byte => byte ^ 0x5a));
        if (out.length !== expected.length || Buffer.compare(out, expected) !== 0) problems.push(`stdout is ${out.length} bytes and not the changed input (${expected.length})`);
        if (!running.lines.some(line => line.includes('ALEF_CONSOLE a line for stderr'))) problems.push('the line for stderr did not arrive');
        if (!running.lines.some(line => line === 'ALEF_MODE windowless console=true')) problems.push('the runtime did not say it runs without a window');
      } catch (error) {
        problems.push(error.message);
      } finally {
        running.stop();
      }
      return { problems, lines: running.lines, site };
    },

    // A service: no window and no console; it answers on a port, and ends by its own word (a request).
    async service() {
      return service({ signal: false });
    },

    // The same service ends on a signal: the page is asked as for `app.quit`, and the code is 128 and the signal.
    async 'service-signal'() {
      if (process.platform === 'win32') {
        console.log('    skipped: a process of Windows ends on a signal without being asked');
        return { problems: [], lines: [] };
      }
      return service({ signal: true });
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

    // Programs of the page: exec (code, streams, shell, timeout, rights) and spawn (streams, kill of
    // the tree); the temp folder of the run is the `fs.read` scope of the app. `cli-substitute` runs
    // with the right `cli.exec:node` substituted: the call hangs and falls with TIMEOUT, nothing starts.
    async cli() {
      const base = mkdtempSync(join(os.tmpdir(), 'alef-e2e-cli-'));
      const work = join(base, 'work');
      mkdirSync(work);
      try {
        return await drive({
          name: 'cli', app: 'modules/system/cli',
          replacements: { WORK: work.replaceAll('\\', '/') },
          targets: { mode: 'allowed', work, outside: base },
          expectedChecks: CLI_CHECKS,
        });
      } finally {
        rmSync(base, { recursive: true, force: true });
      }
    },

    async 'cli-commands'() {
      const site = prepareSite('cli-commands', 'modules/system/cli-commands');
      let running;
      const problems = [];
      try {
        const bin = join(site, 'bin');
        mkdirSync(bin);
        const node = join(bin, process.platform === 'win32' ? 'node.exe' : 'node');
        copyFileSync(process.execPath, node);
        if (process.platform !== 'win32') chmodSync(node, 0o755);
        running = startApp({ exe, args: ['--app', site], verbose,
          env: { ALEF_E2E_CONSENT: 'cli.command:substituted=substitute;*=allow' } });
        await running.waitFor(line => line.includes('ALEF_E2E RESULT'), 120000, 'the declared command verdict');
        const result = verdictOf(running.lines);
        if (result?.[1] !== 'PASS') problems.push(`verdict ${result?.[1]}: ${result?.[2]}`);
        for (const name of CLI_COMMAND_CHECKS) {
          if (!running.lines.some(line => line.includes(` check ${name} ok `))) problems.push(`check without an ok line: ${name}`);
        }
      } catch (error) {
        problems.push(error.message);
      } finally {
        running?.stop();
        if (!running || await running.waitForClosed(30000)) rmSync(site, { recursive: true, force: true });
        else problems.push('runtime did not close within 30 s; temporary site retained');
      }
      return { problems, lines: running?.lines ?? [] };
    },

    async 'cli-substitute'() {
      const base = mkdtempSync(join(os.tmpdir(), 'alef-e2e-cli-sub-'));
      const work = join(base, 'work');
      mkdirSync(work);
      try {
        return await drive({
          name: 'cli-substitute', app: 'modules/system/cli',
          replacements: { WORK: work.replaceAll('\\', '/') },
          targets: { mode: 'substituted', work },
          env: { ALEF_E2E_CONSENT: 'cli.exec:node=substitute;*=allow' },
          expectedChecks: CLI_SUBSTITUTE_CHECKS,
        });
      } finally {
        rmSync(base, { recursive: true, force: true });
      }
    },

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
