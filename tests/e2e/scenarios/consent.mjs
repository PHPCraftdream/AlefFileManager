// SPDX-License-Identifier: MIT OR Apache-2.0
// Scenarios of consent and substitution (docs/stages/m2b-consent.md, "Приёмка"): what the application
// finds when the user decided before the start, and what the launcher does with the decisions.
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import { join } from 'node:path';

import { prepareSite, startApp, verdictOf } from '../lib.mjs';

const CONSENT_CHECKS = [
  'consent-an-environment-variable-is-real-a-stand-in-or-denied',
  'consent-a-denial-is-the-error-of-a-right-the-manifest-never-listed',
  'consent-the-list-of-variables-has-only-what-is-really-given',
  'consent-a-substituted-address-reports-success-and-opens-nothing',
  'consent-a-command-without-a-stand-in-refuses-the-substitute',
  'consent-the-folder-of-the-runtime-is-out-of-reach-of-every-right',
  'consent-the-commands-of-the-permission-window-are-not-the-applications',
];

/** The answers of the user to every right of the `consent` application. */
const ANSWERS = [
  'app.env:ALEF_E2E_ENV_ALLOWED=allow',
  'app.env:ALEF_E2E_ENV_SUBSTITUTED=substitute',
  'app.env:ALEF_E2E_ENV_DENIED=deny',
  'app.env:ALEF_E2E_ENV_UNSET=allow',
  'shell.openExternal:https://allowed.example/*=allow',
  'shell.openExternal:https://stand-in.example/*=substitute',
  'window.create=substitute',
  'fs.read:$TEMP/**=allow',
  'fs.read:$HOME/**=allow',
];

const VARIABLES = { ALEF_E2E_ENV_ALLOWED: 'yes', ALEF_E2E_ENV_SUBSTITUTED: 'really-set', ALEF_E2E_ENV_DENIED: 'secret-31337' };

const linesOf = text => text.split('\n').filter(Boolean);

/** The same file whatever the spelling: links resolved, and the case of Windows ignored. */
const real = path => {
  const resolved = realpathSync.native(path);
  return process.platform === 'win32' ? resolved.toLowerCase() : resolved;
};

/** What the pretending shell was asked, compared as the same files whatever their spelling. */
const shellJudge = (log, note) => () => {
  const asked = existsSync(log) ? linesOf(readFileSync(log, 'utf8')).map(line => JSON.parse(line)) : [];
  const spelled = ({ operation, target }) => [operation, operation === 'openPath' ? real(target) : target];
  const seen = asked.map(spelled);
  const wanted = [['openExternal', 'https://allowed.example/page'], ['openPath', real(note)]];
  return JSON.stringify(seen) === JSON.stringify(wanted)
    ? []
    : [`the shell was asked ${JSON.stringify(seen)}, expected exactly ${JSON.stringify(wanted)}`];
};

export function consentScenarios({ drive, exe, verbose }) {
  /** Runs `alef` to its end with a command line; the answers of the end-to-end runs are off unless given. */
  const cli = (args, env = {}) => {
    // A run of the application from here never shows the permission window: nobody is there to answer it.
    const argv = args[0] === '--app' ? [...args, '--no-prompt'] : args;
    const result = spawnSync(exe, argv, {
      env: { ...process.env, ALEF_E2E: '0', ...env }, encoding: 'utf8', timeout: 60000,
    });
    return { code: result.status, out: result.stdout ?? '', err: result.stderr ?? '' };
  };

  return {
    // The user chose for each right before the start; the page sees the real thing, stand-ins that give
    // nothing away, denials that look like unlisted rights, and the runtime folder stays closed to it.
    async consent() {
      const directory = mkdtempSync(join(os.tmpdir(), 'alef-e2e-consent-'));
      const home = join(directory, 'home');
      const note = join(directory, 'note.txt');
      writeFileSync(note, 'note');
      const log = join(directory, 'shell.log');
      try {
        const result = await drive({
          name: 'consent', app: 'consent', replacements: { SECRETS: 'false' },
          targets: { home, decisions: join(home, 'consent'), note },
          env: { ALEF_HOME: home, ALEF_E2E_CONSENT: ANSWERS.join(';'), ALEF_E2E_SHELL_LOG: log, ...VARIABLES },
          expectedChecks: CONSENT_CHECKS,
          judge: shellJudge(log, note),
        });
        const saved = existsSync(join(home, 'consent')) ? readdirSync(join(home, 'consent')) : [];
        if (saved.length !== 1) result.problems.push(`the decisions were not kept in one file: ${JSON.stringify(saved)}`);
        return result;
      } finally {
        rmSync(directory, { recursive: true, force: true });
      }
    },

    // The user takes one right back and gives another while the application runs: the first applies at
    // once, the second at the next start.
    async narrowing() {
      const problems = [];
      const directory = mkdtempSync(join(os.tmpdir(), 'alef-e2e-narrowing-'));
      const home = join(directory, 'home');
      const environment = { ALEF_HOME: home };
      const app = prepareSite('narrowing', 'consent', {
        replacements: { SECRETS: 'false' },
        targets: { mode: 'narrow', home, decisions: join(home, 'consent'), note: join(directory, 'note.txt') },
      });
      let running;
      try {
        for (const answer of ANSWERS) {
          const [right, decision] = answer.split(/=(?=[a-z]+$)/);
          const set = cli(['permissions', 'set', app, right, decision], environment);
          if (set.code !== 0) problems.push(`set ${right}: ${set.err}`);
        }
        running = startApp({ exe, args: ['--app', app], env: { ...environment, ALEF_E2E_CONSENT: '', ...VARIABLES }, verbose });
        await running.waitFor(line => line.includes('ALEF_E2E narrowing-ready'), 120000, 'the page to be ready');
        const widened = cli(['permissions', 'set', app, 'app.env:ALEF_E2E_ENV_DENIED', 'allow'], environment);
        const narrowed = cli(['permissions', 'set', app, 'app.env:ALEF_E2E_ENV_ALLOWED', 'deny'], environment);
        for (const set of [widened, narrowed]) if (set.code !== 0) problems.push(`set while running: ${set.err}`);
        await running.waitFor(line => line.includes('ALEF_E2E RESULT'), 60000, 'the verdict of the page');
        const result = verdictOf(running.lines);
        if (result?.[1] !== 'PASS') problems.push(`verdict ${result?.[1]}: ${result?.[2]}`);
        if (!running.lines.some(line => / check consent-a-right-taken-back-applies-at-once-and-a-right-given-waits-for-the-next-start ok /.test(line))) {
          problems.push('the check of the page has no ok line');
        }
        const exit = await running.waitForExit(30000);
        if (exit?.code !== 0) problems.push(`the run ended with ${exit?.code ?? 'no exit'}`);
      } catch (error) {
        problems.push(error.message);
      } finally {
        running?.stop();
        rmSync(directory, { recursive: true, force: true });
      }
      return { problems, lines: running?.lines ?? [] };
    },

    // The permission window: a user's clicks played by the page itself decide every right; the window
    // shows all of them, flags the risky one and does not let it start before the risk is confirmed;
    // then the application starts and finds what was clicked.
    async ask() {
      const directory = mkdtempSync(join(os.tmpdir(), 'alef-e2e-ask-'));
      const home = join(directory, 'home');
      const note = join(directory, 'note.txt');
      writeFileSync(note, 'note');
      const log = join(directory, 'shell.log');
      const risky = 'fs.read:$HOME/**';
      const steps = [
        { report: true },
        { all: 'deny' },
        ...ANSWERS.map(answer => {
          const [right, decision] = answer.split(/=(?=[a-z]+$)/);
          return { choose: right, decision };
        }),
        { report: true },
        { confirm: risky },
        { report: true },
        { submit: true },
      ];
      try {
        const result = await drive({
          name: 'ask', app: 'consent', replacements: { SECRETS: 'false' },
          targets: { home, decisions: join(home, 'consent'), note },
          env: {
            ALEF_HOME: home, ALEF_E2E_CONSENT: undefined, ALEF_E2E_CONSENT_UI: JSON.stringify(steps),
            ALEF_E2E_SHELL_LOG: log, ...VARIABLES,
          },
          expectedChecks: CONSENT_CHECKS,
          judge: lines => {
            const problems = shellJudge(log, note)();
            const reports = lines
              .map(line => /^ALEF_E2E consent-ui rows=(\d+) risky=(\[.*?\]) start=(\w+) picked=(\[.*\])$/.exec(line))
              .filter(Boolean)
              .map(([, rows, flagged, start, picked]) => ({ rows: Number(rows), flagged: JSON.parse(flagged), start, picked: JSON.parse(picked) }));
            if (reports.length !== 3) return [...problems, `the window reported ${reports.length} times, expected 3`];
            const [opened, unconfirmed, confirmed] = reports;
            if (opened.rows !== ANSWERS.length) problems.push(`the window shows ${opened.rows} rights, the manifest asks for ${ANSWERS.length}`);
            if (JSON.stringify(opened.flagged) !== JSON.stringify([risky])) problems.push(`risky rights flagged: ${JSON.stringify(opened.flagged)}`);
            if (opened.start !== 'disabled' || !opened.picked.every(item => item.endsWith('=none'))) problems.push('the window lets the application start before anything is decided');
            if (unconfirmed.start !== 'disabled') problems.push('a risky right allowed without its confirmation lets the application start');
            if (confirmed.start !== 'enabled') problems.push('everything is decided and confirmed, yet the application cannot start');
            const saved = existsSync(join(home, 'consent')) ? readdirSync(join(home, 'consent')) : [];
            if (saved.length !== 1) return [...problems, `the decisions were not kept in one file: ${JSON.stringify(saved)}`];
            const kept = JSON.parse(readFileSync(join(home, 'consent', saved[0]), 'utf8')).decisions
              .map(({ permission, scope, decision }) => `${scope === undefined ? permission : `${permission}:${scope}`}=${decision}`)
              .sort();
            if (JSON.stringify(kept) !== JSON.stringify([...ANSWERS].sort())) problems.push(`the store holds ${JSON.stringify(kept)}`);
            return problems;
          },
        });
        return result;
      } finally {
        rmSync(directory, { recursive: true, force: true });
      }
    },

    // Closing the window without deciding: the application does not start and nothing is kept.
    async cancel() {
      const problems = [];
      const directory = mkdtempSync(join(os.tmpdir(), 'alef-e2e-cancel-'));
      const home = join(directory, 'home');
      const app = prepareSite('cancel', 'consent', { replacements: { SECRETS: 'false' } });
      let running;
      try {
        running = startApp({
          exe, args: ['--app', app], verbose,
          env: { ALEF_HOME: home, ALEF_E2E_CONSENT: undefined, ALEF_E2E_CONSENT_UI: JSON.stringify([{ cancel: true }]) },
        });
        const exit = await running.waitForExit(120000);
        if (exit?.code !== 3) problems.push(`the run ended with ${exit?.code ?? 'no exit'}, expected 3`);
        if (!running.lines.some(line => line.includes('closed without a decision'))) problems.push('the refusal does not say that the window was closed');
        if (running.lines.some(line => line.startsWith('ALEF_READY'))) problems.push('the application started without a decision');
        if (existsSync(join(home, 'consent'))) problems.push('a decision was kept although none was made');
      } catch (error) {
        problems.push(error.message);
      } finally {
        running?.stop();
        rmSync(directory, { recursive: true, force: true });
      }
      return { problems, lines: running?.lines ?? [] };
    },

    // The launcher: no decision, no start; the user decides from the command line; a decision is kept;
    // a manifest that asks for more is asked again; `--grant`; `reset`; another folder inherits nothing.
    async decisions() {
      const problems = [];
      const directory = mkdtempSync(join(os.tmpdir(), 'alef-e2e-decisions-'));
      const home = join(directory, 'home');
      const note = join(directory, 'note.txt');
      writeFileSync(note, 'note');
      const log = join(directory, 'shell.log');
      const environment = { ALEF_HOME: home };
      const expect = (what, condition, detail = '') => {
        console.log(`    ${condition ? 'ok  ' : 'FAIL'} ${what}${condition ? '' : `: ${detail}`}`);
        if (!condition) problems.push(`${what}: ${detail}`);
      };
      const site = secrets => prepareSite('decisions', 'consent', { replacements: { SECRETS: secrets } });
      // The page of the consent scenario, started with no answers at all: whatever it finds is what was decided before.
      const run = (secrets, extra = []) => drive({
        name: 'decisions', app: 'consent', replacements: { SECRETS: secrets }, args: extra,
        targets: { home, decisions: join(home, 'consent'), note },
        env: { ...environment, ALEF_E2E_CONSENT: '', ALEF_E2E_SHELL_LOG: log, ...VARIABLES },
        expectedChecks: CONSENT_CHECKS,
      });
      try {
        let app = site('false');

        const refused = cli(['--app', app], environment);
        expect('an application whose rights nobody decided does not start', refused.code === 3, `exit ${refused.code}: ${refused.err}`);
        expect('the refusal names the rights and the way out',
          refused.err.includes('app.env:ALEF_E2E_ENV_ALLOWED') && refused.err.includes('window.create') && refused.err.includes('--grant'), refused.err);
        expect('the refusal opens no window and writes no decision', !refused.err.includes('ALEF_READY') && !existsSync(join(home, 'consent')), refused.err);

        const before = cli(['permissions', 'list', app], environment);
        expect('list shows every right undecided',
          before.code === 0 && linesOf(before.out).filter(line => line.includes('undecided')).length === ANSWERS.length, before.out);
        expect('list without an application has nothing yet', cli(['permissions', 'list'], environment).out.includes('no decisions yet'));

        for (const answer of ANSWERS) {
          const [right, decision] = answer.split(/=(?=[a-z]+$)/);
          const set = cli(['permissions', 'set', app, right, decision], environment);
          if (set.code !== 0) problems.push(`set ${right}: ${set.err}`);
        }
        const unknown = cli(['permissions', 'set', app, 'secrets', 'allow'], environment);
        expect('a right the manifest does not ask for cannot be decided', unknown.code === 2 && unknown.err.includes('does not ask for secrets'), unknown.err);
        const listed = cli(['permissions', 'list', app], environment).out;
        expect('list shows what was decided',
          listed.includes('substitute\tapp.env:ALEF_E2E_ENV_SUBSTITUTED') && listed.includes('deny\tapp.env:ALEF_E2E_ENV_DENIED') && !listed.includes('undecided'), listed);
        expect('list without an application names it',
          cli(['permissions', 'list'], environment).out.startsWith('org.alef.e2e.modules.consent\t'));

        // The decisions are kept: the start asks nothing, the stand-ins and denials hold.
        const kept = await run('false');
        expect('the decisions made from the command line hold at the next start', kept.problems.length === 0, kept.problems.join('; '));
        const asked = shellJudge(log, note)();
        expect('and the stand-in opened nothing', asked.length === 0, asked.join('; '));

        // The manifest asks for one more right: only that one is asked.
        app = site('true');
        const more = cli(['--app', app], environment);
        expect('a right the manifest added is asked again, and only that one',
          more.code === 3 && more.err.includes('secrets') && !more.err.includes('app.env'), `exit ${more.code}: ${more.err}`);

        // `--grant` decides what is not decided yet and keeps what was.
        const granted = await run('true', ['--grant', 'deny']);
        expect('--grant lets the application start with the new right decided', granted.problems.length === 0, granted.problems.join('; '));
        const after = cli(['permissions', 'list', app], environment).out;
        expect('the new right got the granted decision and the old ones were kept',
          after.includes('deny\tsecrets') && after.includes('substitute\tapp.env:ALEF_E2E_ENV_SUBSTITUTED') && after.includes('allow\tapp.env:ALEF_E2E_ENV_ALLOWED'), after);

        // Another folder with the same id inherits nothing.
        const other = prepareSite('decisions-other', 'consent', { replacements: { SECRETS: 'true' } });
        const inherited = cli(['permissions', 'list', other], environment).out;
        expect('another folder with the same id has decisions of its own', !inherited.includes('allow\t') && !inherited.includes('deny\t'), inherited);

        // `reset`: the next start asks again.
        const reset = cli(['permissions', 'reset', app], environment);
        expect('reset forgets the decisions and the next start asks again',
          reset.code === 0 && cli(['--app', app], environment).code === 3, reset.out + reset.err);
        expect('and the application is gone from the list', cli(['permissions', 'list'], environment).out.includes('no decisions yet'));
      } catch (error) {
        problems.push(error.message);
      } finally {
        rmSync(directory, { recursive: true, force: true });
      }
      return { problems, lines: [] };
    },
  };

}
