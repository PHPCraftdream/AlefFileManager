// SPDX-License-Identifier: MIT OR Apache-2.0
import { createHash, createPublicKey, sign, randomUUID } from 'node:crypto';
import { lstat, readdir, realpath, open, rename, unlink } from 'node:fs/promises';
import { constants } from 'node:fs';
import path from 'node:path';
import { keyHome, loadKey, noSymlinks } from './keys.mjs';
import { validateName, writeZip } from './zip.mjs';

const MAX_FILE = 512 * 1024 * 1024;
const MAX_HASHES = 16 * 1024 * 1024;
const MAX_TOTAL = 2 * 1024 * 1024 * 1024;
// Canonical signature text: header 15 + key line 49 + signature line 93.
const SIGNATURE_SIZE = 15 + 49 + 93;
const EXCLUDES = new Set(['.git', 'node_modules']);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const check = (condition, message) => { if (!condition) throw new Error(`Pack: ${message}`); };
function within(parent, child) {
  const relative = path.relative(parent, child);
  return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
}
async function boundedRead(file) {
  await noSymlinks(file.path);
  const handle = await open(file.path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const stat = await handle.stat();
    check(stat.isFile() && stat.size === file.size && stat.dev === file.dev && stat.ino === file.ino, `${file.name}: file changed`);
    const bytes = Buffer.alloc(file.size); let size = 0;
    while (size < bytes.length) {
      const { bytesRead } = await handle.read(bytes, size, bytes.length - size, size);
      check(bytesRead > 0, `${file.name}: file shrank`); size += bytesRead;
    }
    const extra = Buffer.alloc(1);
    check((await handle.read(extra, 0, 1, size)).bytesRead === 0, `${file.name}: file grew`);
    check((await handle.stat()).size === file.size, `${file.name}: file changed`);
    // Copied private PEMs are secrets too, even outside the configured home.
    check(!bytes.includes(Buffer.from('-----BEGIN PRIVATE KEY-----')) && !bytes.includes(Buffer.from('-----BEGIN ENCRYPTED PRIVATE KEY-----')) && !bytes.includes(Buffer.from('-----BEGIN RSA PRIVATE KEY-----')) && !bytes.includes(Buffer.from('-----BEGIN EC PRIVATE KEY-----')) && !bytes.includes(Buffer.from('-----BEGIN OPENSSH PRIVATE KEY-----')), `${file.name}: private key forbidden`);
    return bytes;
  } finally { await handle.close(); }
}
export async function pack(dir, options = {}) {
  check(!(options.unsigned && options.key !== undefined), 'key and unsigned are mutually exclusive');
  check(options.unsigned === undefined || typeof options.unsigned === 'boolean', 'unsigned must be boolean');
  const include = options.include ?? [];
  check(Array.isArray(include) && include.every(name => EXCLUDES.has(name) || name === '.alef-runtime'), 'include must be an array of excluded basenames');
  const root = path.resolve(dir);
  await noSymlinks(root);
  check((await lstat(root)).isDirectory(), 'input must be a directory');
  const actualRoot = await realpath(root);
  const home = keyHome(options);
  let actualHome;
  try { actualHome = await realpath(home); } catch (error) { if (error.code !== 'ENOENT') throw error; actualHome = home; }
  check(!within(actualHome, actualRoot), 'input is inside key home');
  const out = path.resolve(options.out ?? path.join(path.dirname(root), `${path.basename(root)}.alef`));
  await noSymlinks(out);
  check((await lstat(path.dirname(out))).isDirectory(), 'output parent must be a directory');
  const actualOut = path.join(await realpath(path.dirname(out)), path.basename(out));
  check(!within(actualHome, actualOut), 'output is inside key home');
  const signed = !options.unsigned; const files = [];
  let total = 0; let hashSize = 0;
  async function walk(folder, prefix = '') {
    await noSymlinks(folder);
    for (const basename of await readdir(folder)) {
      const filename = path.join(folder, basename); const name = prefix + basename;
      if (path.relative(actualOut, filename) === '' || within(actualHome, filename) || basename.toLowerCase() === '.alef-runtime') continue;
      if (EXCLUDES.has(basename) && !include.includes(basename)) continue;
      try { validateName(name); }
      catch (error) { throw new Error(`${name}: ${error.message}`, { cause: error }); }
      check(name.split('/')[0].toLowerCase() !== 'meta-inf', `${name}: reserved META-INF input`);
      check(name.toLowerCase() !== 'alef.ktav' || name === 'alef.ktav', `${name}: noncanonical manifest name`);
      const stat = await lstat(filename);
      check(!stat.isSymbolicLink(), `${name}: symlink forbidden`);
      if (stat.isDirectory()) { await walk(filename, `${name}/`); continue; }
      check(stat.isFile(), `${name}: special file forbidden`);
      check(Number.isSafeInteger(stat.size) && stat.size <= MAX_FILE, `${name}: file size limit`);
      files.push({ path: filename, name, size: stat.size, dev: stat.dev, ino: stat.ino });
      check(files.length + (signed ? 2 : 1) <= 10000, 'entry count limit');
      total += stat.size; hashSize += 67 + Buffer.byteLength(name);
      check(hashSize <= MAX_HASHES, 'hashes file size limit');
      check(total + hashSize + (signed ? SIGNATURE_SIZE : 0) <= MAX_TOTAL, 'total size limit');
    }
  }
  await walk(actualRoot);
  const names = new Set(); const prefixes = new Set();
  for (const file of files) {
    const key = file.name.toLowerCase();
    check(!names.has(key), `${file.name}: case-insensitive duplicate name`);
    check(!prefixes.has(key), `${file.name}: file path-prefix conflict`);
    const segments = key.split('/');
    for (let index = 1; index < segments.length; index++) {
      const prefix = segments.slice(0, index).join('/');
      check(!names.has(prefix), `${file.name}: file path-prefix conflict`);
      prefixes.add(prefix);
    }
    names.add(key);
  }
  check(files.some(file => file.name === 'alef.ktav'), 'required regular root alef.ktav is missing');
  const privateKey = signed ? await loadKey(options.key ?? 'default', options) : undefined;
  files.sort((a, b) => Buffer.compare(Buffer.from(a.name), Buffer.from(b.name)));
  const entries = []; const lines = [];
  for (const file of files) {
    const data = await boundedRead(file);
    entries.push({ name: file.name, data }); lines.push(`${hash(data)}  ${file.name}\n`);
  }
  const hashes = Buffer.from(lines.join(''));
  entries.push({ name: 'META-INF/hashes', data: hashes });
  let fingerprint;
  if (signed) {
    const raw = createPublicKey(privateKey).export({ format: 'der', type: 'spki' }).subarray(-32);
    const signature = sign(null, Buffer.concat([Buffer.from('alef-package-v1\n'), hashes]), privateKey);
    entries.push({ name: 'META-INF/signature', data: Buffer.from(`alef-package 1\nkey ${raw.toString('base64')}\nsig ${signature.toString('base64')}\n`) });
    fingerprint = hash(raw);
  }
  const bytes = writeZip(entries);
  const temp = path.join(path.dirname(out), `.alef-pack-${randomUUID()}`);
  await noSymlinks(out);
  const handle = await open(temp, 'wx', 0o600);
  try {
    try { await handle.writeFile(bytes); await handle.sync(); } finally { await handle.close(); }
    await noSymlinks(out); await rename(temp, out);
  } finally { await unlink(temp).catch(error => { if (error.code !== 'ENOENT') throw error; }); }
  return Object.freeze({ out, count: entries.length, status: signed ? 'Signed' : 'Unsigned', ...(signed ? { fingerprint } : {}) });
}
