// SPDX-License-Identifier: MIT OR Apache-2.0
// Scenarios of the data modules (docs/stages/m3-data.md, "Приёмка"): `fs` against the real disk, with
// the scope allowed and with a stand-in the user chose; `store` across two runs of one application.
import { createHash, randomBytes } from 'node:crypto';
import { closeSync, createReadStream, existsSync, ftruncateSync, mkdirSync, mkdtempSync, openSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync, writeSync } from 'node:fs';
import os from 'node:os';
import { join } from 'node:path';

import { prepareSite, startApp, verdictOf } from '../lib.mjs';

const FS_CHECKS = [
  'fs-text-and-bytes-round-trip-with-unicode', 'fs-stat-readdir-rename-copy-and-remove',
  'fs-outside-the-scope-is-denied-however-the-path-is-spelled', 'fs-a-link-that-leads-out-of-the-scope-leads-nowhere',
  'fs-a-scratch-file-and-a-picked-file-need-no-scope', 'fs-a-whole-file-is-for-small-files',
  'fs-a-handle-reads-and-writes-pieces', 'fs-a-big-file-is-copied-through-streams-and-arrives-whole',
  'fs-a-big-folder-comes-in-batches', 'fs-watch-tells-create-modify-and-remove',
];

const STORE_ID = 'org.alef.e2e.modules.store';
const STORE_CHECKS = {
  write: [
    'store-values-of-every-json-kind-come-back', 'store-keys-come-in-order-by-prefix-and-delete',
    'store-areas-are-apart-and-named-with-care', 'store-a-value-is-up-to-what-a-call-carries', 'store-flush-waits-for-the-disk',
  ],
  read: ['store-what-the-first-run-kept-is-there-after-the-restart', 'store-it-goes-on-working-after-the-restart'],
};

/** Where the runtime keeps the data of the application of this id: the folder `$APPDATA` names. */
function appDataOf(id) {
  const home = os.homedir();
  if (process.platform === 'win32') return join(process.env.LOCALAPPDATA ?? join(home, 'AppData', 'Local'), id);
  if (process.platform === 'darwin') return join(home, 'Library', 'Application Support', id);
  return join(process.env.XDG_DATA_HOME ?? join(home, '.local', 'share'), id);
}

const COPY_SIZE = 96 * 1024 * 1024 + 4321;
const MANY = 2500;

/** The sha-256 of a file, read as a stream. */
function sha256(path) {
  return new Promise((resolvePromise, reject) => {
    const hash = createHash('sha256');
    createReadStream(path).on('data', chunk => hash.update(chunk)).on('error', reject).on('end', () => resolvePromise(hash.digest('hex')));
  });
}

const SUBSTITUTE_CHECKS = [
  'fs-a-stand-in-starts-as-an-empty-folder', 'fs-a-stand-in-keeps-what-is-written-and-answers-like-a-disk',
  'fs-a-stand-in-refuses-what-the-scope-refuses',
];

/** Every file below `directory`, as paths relative to it. */
function filesBelow(directory) {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true, recursive: true })
    .filter(entry => entry.isFile())
    .map(entry => join(entry.parentPath ?? entry.path, entry.name).slice(directory.length + 1));
}

/** The folders the page works in: a scope, a folder beside it, a picked file, a link and a big file. */
function prepare() {
  const base = mkdtempSync(join(os.tmpdir(), 'alef-e2e-fs-'));
  const root = join(base, 'root');
  const outside = join(base, 'outside');
  mkdirSync(root);
  mkdirSync(outside);
  writeFileSync(join(outside, 'secret.txt'), 'secret');
  writeFileSync(join(base, 'picked.txt'), 'picked');
  writeFileSync(join(base, 'neighbour.txt'), 'neighbour');
  writeFileSync(join(root, 'real.txt'), 'real');
  mkdirSync(join(root, 'real-folder'));
  let link = null;
  try {
    symlinkSync(join(outside, 'secret.txt'), join(root, 'link'));
    link = join(root, 'link');
  } catch {
    // an account that may not make links: the page says so
  }
  const many = join(root, 'many');
  mkdirSync(many);
  for (let index = 0; index < MANY; index += 1) writeFileSync(join(many, `f${String(index).padStart(5, '0')}.txt`), 'x');
  const copySource = join(root, 'copy-source.bin');
  const source = openSync(copySource, 'w');
  for (let written = 0; written < COPY_SIZE; written += 1024 * 1024) writeSync(source, randomBytes(Math.min(1024 * 1024, COPY_SIZE - written)));
  closeSync(source);
  const big = join(root, 'big.bin');
  const handle = openSync(big, 'w');
  ftruncateSync(handle, 64 * 1024 * 1024 + 1);
  closeSync(handle);
  return {
    base, root, link, big, many, copySource, outside,
    secret: join(outside, 'secret.txt'), picked: join(base, 'picked.txt'),
    scope: `${root.replaceAll('\\', '/')}/**`,
  };
}

export function dataScenarios({ drive, exe, verbose }) {
  return {
    // The user allowed the scope: everything the page does is real and stays inside it.
    async fs() {
      const here = prepare();
      try {
        return await drive({
          name: 'fs', app: 'modules/data/fs', replacements: { ROOT: here.scope.slice(0, -3) },
          targets: {
            mode: 'real', root: here.root, outside: here.secret, outsideName: 'outside', outsideFolder: here.outside, link: here.link,
            big: here.big, many: here.many, manyCount: MANY, copySource: here.copySource, copySize: COPY_SIZE,
          },
          env: { ALEF_HOME: join(here.base, 'home'), ALEF_E2E_DIALOGS: JSON.stringify([{ open: [here.picked] }]) },
          expectedChecks: FS_CHECKS,
          judge: async () => {
            const problems = [];
            const copied = join(here.root, 'work', 'copied.bin');
            if (!existsSync(copied) || await sha256(copied) !== await sha256(here.copySource)) problems.push('the copy made through streams is not the file');
            const note = join(here.root, 'work', 'note.txt');
            if (!existsSync(note) || readFileSync(note, 'utf8') !== 'héllo — мир 🌍!') problems.push('the note is not on the disk as the page wrote it');
            if (readFileSync(here.secret, 'utf8') !== 'secret') problems.push('the file outside the scope was changed');
            if (readFileSync(here.picked, 'utf8') !== 'picked') problems.push('the picked file was changed');
            if (here.link && existsSync(here.link)) problems.push('the link is still there');
            if (here.link && !existsSync(here.secret)) problems.push('removing the link removed what it led to');
            if (!existsSync(join(here.root, 'work', 'x2', 'y', 'copy.txt'))) problems.push('the copied folder is missing');
            return problems;
          },
        });
      } finally {
        rmSync(here.base, { recursive: true, force: true });
      }
    },

    // The user chose a stand-in for the scope: the page finds an empty folder and keeps its writes in it;
    // the real folder is as it was, and what the page wrote lies in the folder of the runtime.
    async 'fs-substitute'() {
      const here = prepare();
      const home = join(here.base, 'home');
      try {
        const before = filesBelow(here.root).sort();
        const result = await drive({
          name: 'fs-substitute', app: 'modules/data/fs', replacements: { ROOT: here.scope.slice(0, -3) },
          targets: { mode: 'substituted', root: here.root, outside: here.secret, outsideName: 'outside' },
          env: {
            ALEF_HOME: home,
            ALEF_E2E_CONSENT: `fs.read:${here.scope}=substitute;fs.write:${here.scope}=substitute`,
          },
          expectedChecks: SUBSTITUTE_CHECKS,
          judge: () => {
            const problems = [];
            const after = filesBelow(here.root).sort();
            if (JSON.stringify(after) !== JSON.stringify(before)) problems.push(`the real folder changed: ${JSON.stringify(before)} -> ${JSON.stringify(after)}`);
            const kept = filesBelow(join(home, 'shadow'));
            for (const name of ['renamed.txt', 'copied.txt']) {
              if (!kept.some(path => path.endsWith(name))) problems.push(`${name} is not in the folder of the stand-in`);
            }
            if (!kept.some(path => path.endsWith(join('a', 'b', 'deep.txt')))) problems.push('the nested file is not in the folder of the stand-in');
            return problems;
          },
        });
        return result;
      } finally {
        rmSync(here.base, { recursive: true, force: true });
      }
    },

    // The store keeps what was written for the next run: the application runs twice, a new process each
    // time, on a data folder that is empty at the start and removed at the end.
    async store() {
      const site = prepareSite('store', 'modules/data/store');
      const problems = [];
      const data = appDataOf(STORE_ID);
      const wipe = () => {
        if (data.endsWith(STORE_ID)) rmSync(data, { recursive: true, force: true });
      };
      const same = (left, right) => (process.platform === 'win32' ? left.toLowerCase() === right.toLowerCase() : left === right);
      const run = async step => {
        const running = startApp({ exe, args: ['--app', site, '--', `--step=${step}`], verbose });
        try {
          await running.waitFor(line => line.includes('ALEF_E2E RESULT'), 120000, `the verdict of ${step}`, { survivesExit: true });
          const result = verdictOf(running.lines);
          if (result?.[1] !== 'PASS') problems.push(`${step}: ${result?.[2] || 'the run failed'}`);
          const exit = await running.waitForExit(30000);
          if (exit?.code !== 0) problems.push(`${step}: the run ended with ${exit?.code ?? 'no exit'}`);
        } catch (error) {
          problems.push(`${step}: ${error.message}`);
        } finally {
          running.stop();
        }
        const done = running.lines.map(line => /ALEF_E2E check (\S+) ok/.exec(line)?.[1]).filter(Boolean);
        for (const name of STORE_CHECKS[step]) if (!done.includes(name)) problems.push(`${step}: no ok line for ${name}`);
        const reported = running.lines.map(line => /ALEF_E2E path appData (.+)$/.exec(line)?.[1]).find(Boolean);
        if (!reported || !same(reported.trim(), data)) problems.push(`${step}: the data folder is ${reported}, the runner expected ${data}`);
      };
      try {
        wipe();
        await run('write');
        await run('read');
        if (!existsSync(join(data, 'store'))) problems.push('the store is not in the data folder of the application');
      } finally {
        wipe();
      }
      return { problems, lines: [] };
    },
  };
}
