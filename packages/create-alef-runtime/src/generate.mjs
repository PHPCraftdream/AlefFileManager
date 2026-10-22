// SPDX-License-Identifier: MIT OR Apache-2.0
import { lstat, readdir, readFile, realpath, mkdir, open, unlink, rmdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { validateOptions } from './validate.mjs';
import { iconPng } from './icon.mjs';

// Rsbuild is pinned to the exact version the template was verified with. Vite 6 supports Node 22;
// use its major range rather than inventing an unobserved patch version.
export const versions = Object.freeze({ API: '^0.1.0', TOOLS: '^0.1.0', RSBUILD: '2.2.9', VITE: '^6.0.0' });
const templates = fileURLToPath(new URL('../templates/', import.meta.url));
const moduleScript = '<script type="module" src="/src/main.js"></script>';
const escapeHtml = value => value.replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);

const ID_PATTERN = /^[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)*$/;
const NAME_PATTERN = /^[\p{L}\p{N} ._!?()-]+$/u;
const PACKAGE_PATTERN = /^[a-z0-9][a-z0-9._-]*$/;
const HOSTILE = /\{\{|\}\}|\p{Cc}/u; // '{{'/'}}' or any control character (incl. newlines)

// Guard every value against its place's shape before substitution, so a hostile
// value can never smuggle template tokens or control characters into output.
function guard(token, value, pattern) {
  if (typeof value !== 'string') throw new Error(`Invalid value for template token ${token}: a string is required.`);
  if (HOSTILE.test(value)) throw new Error(`Invalid value for template token ${token}: contains placeholders, newlines, or control characters.`);
  if (pattern && !pattern.test(value)) throw new Error(`Invalid value for template token ${token}: ${value}`);
}

export function render(text, values, html = false) {
  const replacements = { ID: values.id, NAME: html ? escapeHtml(values.name) : values.name, PACKAGE: values.package, ...versions };
  guard('ID', replacements.ID, ID_PATTERN);
  guard('NAME', replacements.NAME, NAME_PATTERN);
  guard('PACKAGE', replacements.PACKAGE, PACKAGE_PATTERN);
  for (const token of ['API', 'TOOLS', 'RSBUILD', 'VITE']) guard(token, replacements[token], null);
  const result = text.replace(/\{\{([^{}]+)\}\}/g, (_, token) => {
    if (!Object.hasOwn(replacements, token)) throw new Error(`Unknown template token: ${token}`);
    return replacements[token];
  });
  if (result.includes('{{') || result.includes('}}')) throw new Error('Unresolved template placeholder.');
  return result;
}

async function renderTemplates(values) {
  const files = new Map();
  async function walk(root, relative = '') {
    for (const entry of await readdir(path.join(root, relative), { withFileTypes: true })) {
      const name = relative ? `${relative}/${entry.name}` : entry.name;
      if (entry.isDirectory()) await walk(root, name);
      else if (entry.isFile()) {
        const output = name === 'gitignore' ? '.gitignore' : name.replace(/\.template$/, '');
        let text = render(await readFile(path.join(root, name), 'utf8'), values, output.endsWith('.html'));
        // Vite needs this source script; Rsbuild injects its compiled entry itself.
        if (output === 'index.html' && values.bundler === 'rsbuild') {
          if (!text.includes(moduleScript)) throw new Error('Missing Vite source script in HTML template.');
          text = text.replace(moduleScript, '');
        }
        files.set(output, Buffer.from(text));
      } else throw new Error(`Template is not a regular file/directory: ${name}`);
    }
  }
  await walk(path.join(templates, 'base'));
  await walk(path.join(templates, values.bundler));
  JSON.parse(files.get('package.json').toString());
  files.set('icon.png', iconPng());
  return files;
}

async function statOrMissing(file) {
  try { return await lstat(file); }
  catch (error) { if (error.code === 'ENOENT') return undefined; throw error; }
}
const same = (a, b) => a && b && a.dev === b.dev && a.ino === b.ino && a.isDirectory() === b.isDirectory() && a.isFile() === b.isFile();

export async function generate(options, cwd = process.cwd()) {
  const values = validateOptions(options);
  const files = await renderTemplates(values); // All validation/rendering precedes writes.
  const root = await realpath(cwd); // Accept symlinked parents and Windows 8.3 aliases.
  const target = path.join(root, values.directory);
  const before = await statOrMissing(target);
  if (before && (before.isSymbolicLink() || !before.isDirectory())) throw new Error('Target must be a real directory, not a symlink.');
  if (before && (await readdir(target)).length) throw new Error('Target directory is not empty; no files were written.');
  const ownedFiles = []; const ownedDirs = [];
  let targetStat = before;
  async function ensureTarget() {
    const current = await lstat(target);
    if (current.isSymbolicLink() || !same(current, targetStat)) throw new Error('Target directory changed during generation.');
  }
  // Exclusive creation and identity-checked cleanup are best effort, not an atomic
  // defense against malicious simultaneous directory replacement.
  try {
    if (!before) {
      await mkdir(target);
      targetStat = await lstat(target);
      ownedDirs.push({ file: target, stat: targetStat });
    }
    await ensureTarget();
    if ((await readdir(target)).length) throw new Error('Target directory is no longer empty.');
    for (const [relative, content] of files) {
      await ensureTarget();
      const parts = relative.split('/');
      let parent = target;
      for (const part of parts.slice(0, -1)) {
        parent = path.join(parent, part);
        let stat = await statOrMissing(parent);
        const owned = ownedDirs.find(item => item.file === parent);
        if (!stat) {
          await mkdir(parent);
          stat = await lstat(parent);
          ownedDirs.push({ file: parent, stat });
        } else if (!owned || stat.isSymbolicLink() || !same(stat, owned.stat)) {
          throw new Error('Output subdirectory already exists or changed.');
        }
      }
      const file = path.join(target, ...parts);
      const handle = await open(file, 'wx');
      try {
        ownedFiles.push({ file, stat: await handle.stat() });
        await handle.writeFile(content);
      } finally { await handle.close(); }
    }
    return { ...values, directory: target, directoryName: values.directory, target };
  } catch (error) {
    const cleanupErrors = [];
    for (const item of [...ownedFiles].reverse()) {
      try { if (same(await statOrMissing(item.file), item.stat)) await unlink(item.file); }
      catch (cleanupError) { cleanupErrors.push(cleanupError.message); }
    }
    for (const item of [...ownedDirs].reverse()) {
      try { if (same(await statOrMissing(item.file), item.stat)) await rmdir(item.file); }
      catch (cleanupError) { cleanupErrors.push(cleanupError.message); }
    }
    if (cleanupErrors.length) throw new Error(`${error.message}; cleanup incomplete: ${cleanupErrors.join('; ')}`, { cause: error });
    throw error;
  }
}
