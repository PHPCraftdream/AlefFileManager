// SPDX-License-Identifier: MIT OR Apache-2.0
// Scenarios of the data modules (docs/stages/m3-data.md, "Приёмка"): `fs` against the real disk, with
// the scope allowed and with a stand-in the user chose.
import { closeSync, existsSync, ftruncateSync, mkdirSync, mkdtempSync, openSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import { join } from 'node:path';

const FS_CHECKS = [
  'fs-text-and-bytes-round-trip-with-unicode', 'fs-stat-readdir-rename-copy-and-remove',
  'fs-outside-the-scope-is-denied-however-the-path-is-spelled', 'fs-a-link-that-leads-out-of-the-scope-leads-nowhere',
  'fs-a-scratch-file-and-a-picked-file-need-no-scope', 'fs-a-whole-file-is-for-small-files',
];

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
  const big = join(root, 'big.bin');
  const handle = openSync(big, 'w');
  ftruncateSync(handle, 64 * 1024 * 1024 + 1);
  closeSync(handle);
  return {
    base, root, link, big,
    secret: join(outside, 'secret.txt'), picked: join(base, 'picked.txt'),
    scope: `${root.replaceAll('\\', '/')}/**`,
  };
}

export function dataScenarios({ drive }) {
  return {
    // The user allowed the scope: everything the page does is real and stays inside it.
    async fs() {
      const here = prepare();
      try {
        return await drive({
          name: 'fs', app: 'modules/data/fs', replacements: { ROOT: here.scope.slice(0, -3) },
          targets: { mode: 'real', root: here.root, outside: here.secret, outsideName: 'outside', link: here.link, big: here.big },
          env: { ALEF_HOME: join(here.base, 'home'), ALEF_E2E_DIALOGS: JSON.stringify([{ open: [here.picked] }]) },
          expectedChecks: FS_CHECKS,
          judge: () => {
            const problems = [];
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
  };
}
