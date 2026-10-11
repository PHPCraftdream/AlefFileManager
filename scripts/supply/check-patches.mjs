import { createHash, randomUUID } from 'node:crypto';
import { promises as fs } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { gunzipSync } from 'node:zlib';

const ROOT = fileURLToPath(new URL('../../', import.meta.url));
const REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index';
const MAX = 64 * 1024 * 1024;
export class CheckError extends Error {
  constructor(message, code = 1) { super(message); this.code = code; }
}
function requireCheck(ok, message) { if (!ok) throw new CheckError(message); }
export function sha256(bytes) { return createHash('sha256').update(bytes).digest('hex'); }
export function normalize(bytes) {
  const out = Buffer.alloc(bytes.length);
  let size = 0;
  for (let i = 0; i < bytes.length; i++) {
    if (bytes[i] === 13 && bytes[i + 1] === 10) continue;
    out[size++] = bytes[i];
  }
  return out.subarray(0, size);
}
export function safePath(value) {
  return typeof value === 'string' && value.length > 0 && value.length < 1024
    && !/[\\:]/u.test(value) && ![...value].some(char => char.charCodeAt(0) < 32 || char.charCodeAt(0) === 127)
    && value.split('/').every(part => part && part !== '.' && part !== '..'
      && !/[. ]$/u.test(part) && !/^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/iu.test(part));
}
export function readInventory(text) {
  const blocks = [...text.matchAll(/```json\s*\n([\s\S]*?)\n```/gu)];
  requireCheck(blocks.length === 1, 'docs/PATCHES.md: expected one JSON inventory');
  let doc;
  try { doc = JSON.parse(blocks[0][1]); } catch { throw new CheckError('docs/PATCHES.md: invalid JSON'); }
  requireCheck(doc.schemaVersion === 1 && doc.comparison === 'tracked-working-tree-vs-verified-crate'
    && doc.normalization === 'CRLF-to-LF-bytes-only'
    && JSON.stringify(doc.ignoredPublicationMetadata) === '[".cargo_vcs_info.json"]'
    && Array.isArray(doc.crates) && doc.crates.length > 0, 'docs/PATCHES.md: invalid schema');
  const names = new Set();
  const roots = new Set();
  const hash = value => typeof value === 'string' && /^[a-f0-9]{64}$/u.test(value);
  for (const crate of doc.crates) {
    requireCheck(/^[a-z][a-z0-9-]*$/u.test(crate.name) && /^\d+\.\d+\.\d+$/u.test(crate.version)
      && crate.root === `backend/patches/${crate.name}` && hash(crate.sha256)
      && !names.has(crate.name) && !roots.has(crate.root), 'docs/PATCHES.md: invalid/duplicate crate');
    names.add(crate.name); roots.add(crate.root);
    requireCheck(crate.primaryLock?.path === 'backend/Cargo.lock' && crate.primaryLock.version === crate.version
      && crate.primaryLock.source === null && crate.primaryLock.checksum === null
      && crate.archiveUrl === `https://static.crates.io/crates/${crate.name}/${crate.version}/download`
      && Array.isArray(crate.checksumSources) && Array.isArray(crate.files), `${crate.root}: invalid contract`);
    const registry = crate.checksumSources.filter(s => s.kind === 'registry-exact-version');
    const indexPath = crate.name.length === 1 ? `1/${crate.name}` : crate.name.length === 2 ? `2/${crate.name}`
      : crate.name.length === 3 ? `3/${crate.name[0]}/${crate.name}` : `${crate.name.slice(0, 2)}/${crate.name.slice(2, 4)}/${crate.name}`;
    requireCheck(registry.length === 1 && registry[0].url === `https://index.crates.io/${indexPath}`
      && registry[0].version === crate.version && registry[0].field === 'cksum', `${crate.root}: invalid registry source`);
    for (const source of crate.checksumSources.filter(s => s.kind !== 'registry-exact-version')) {
      requireCheck(source.kind === 'lock' && safePath(source.path) && source.path.startsWith('backend/patches/')
        && source.name === crate.name && source.version === crate.version && source.source === REGISTRY
        && source.field === 'checksum', `${crate.root}: invalid lock source`);
    }
    const files = new Set();
    for (const file of crate.files) {
      requireCheck(safePath(file.path) && !files.has(file.path) && ['added', 'changed', 'removed'].includes(file.status)
        && typeof file.tracked === 'boolean' && file.tracked === (file.status !== 'removed')
        && (file.status === 'removed' ? file.normalizedSha256 === undefined : hash(file.normalizedSha256))
        && (file.ignored === undefined || (file.ignored === true && file.path === '.cargo_vcs_info.json')),
      `${crate.root}: invalid inventory file`);
      files.add(file.path);
    }
  }
  return doc;
}
// Cargo's generated lock format: only package string fields are needed, not a general TOML parser.
export function lockPackages(text) {
  return text.split(/^\[\[package\]\]\s*$/mu).slice(1).map(block => {
    const fields = {};
    for (const match of block.matchAll(/^(name|version|source|checksum)\s*=\s*"([^"\r\n]*)"\s*$/gmu)) {
      requireCheck(fields[match[1]] === undefined, 'Cargo.lock: duplicate package field');
      fields[match[1]] = match[2];
    }
    return fields;
  });
}
export function validateLocks(crate, locks) {
  const primary = locks.get('backend/Cargo.lock')?.filter(p => p.name === crate.name);
  requireCheck(primary?.length === 1 && primary[0].version === crate.version
    && primary[0].source === undefined && primary[0].checksum === undefined,
  `backend/Cargo.lock: ${crate.name} primary version/path package mismatch`);
  for (const [file, packages] of locks) {
    for (const p of packages.filter(pkg => pkg.name === crate.name && pkg.version === crate.version && pkg.source === REGISTRY)) {
      requireCheck(p.checksum === crate.sha256, `${file}: ${crate.name} checksum mismatch`);
    }
  }
  for (const source of crate.checksumSources.filter(s => s.kind === 'lock')) {
    requireCheck(locks.get(source.path)?.some(p => p.name === crate.name && p.version === crate.version
      && p.source === source.source && p.checksum === crate.sha256), `${source.path}: ${crate.name} missing checksum source`);
  }
}
export function compareFiles(crate, original, local, tracked) {
  const expected = new Map(crate.files.filter(f => !f.ignored).map(f => [f.path, f]));
  const paths = new Set([...original.keys(), ...local.keys(), ...expected.keys()]);
  for (const file of [...paths].sort()) {
    if (file === '.cargo_vcs_info.json') continue;
    const before = original.get(file), after = local.get(file);
    const status = before === undefined ? (after === undefined ? 'unchanged' : 'added')
      : after === undefined ? 'removed' : normalize(before).equals(normalize(after)) ? 'unchanged' : 'changed';
    const entry = expected.get(file);
    const label = `${crate.root}/${file}`;
    requireCheck(status === (entry?.status ?? 'unchanged'), `${label}: status ${status}, expected ${entry?.status ?? 'unchanged'}`);
    requireCheck(!entry || tracked.has(file) === entry.tracked, `${label}: tracking mismatch`);
    if (entry?.normalizedSha256) requireCheck(sha256(normalize(after)) === entry.normalizedSha256, `${label}: content pin mismatch`);
  }
}
function run(command, args, cwd, input, allowed = [0]) {
  const result = spawnSync(command, args, { cwd, input, timeout: 20_000, maxBuffer: MAX, windowsHide: true });
  if (result.error || !allowed.includes(result.status)) throw new CheckError(`${command}: unavailable or failed`, 2);
  return result.stdout;
}
export function gitState(root) {
  const tracked = new Set(run('git', ['ls-files', '-z', '--', 'backend/patches'], root).toString('utf8').split('\0').filter(Boolean));
  const ignored = run('git', ['check-ignore', '--no-index', '-z', '--stdin'], root, Buffer.from('backend/patches/winit/Cargo.lock\0'), [0, 1]).toString('utf8');
  return { tracked, winitIgnored: ignored === 'backend/patches/winit/Cargo.lock\0' };
}
async function read(root, file) {
  try { return await fs.readFile(path.join(root, file)); }
  catch (error) {
    if (error.code === 'ENOENT') throw new CheckError(`${file}: required file missing`);
    throw error;
  }
}
async function walk(root, relative = '') {
  const files = new Map();
  const visit = async rel => {
    const full = path.join(root, rel);
    const stat = await fs.lstat(full);
    requireCheck(!stat.isSymbolicLink() && (stat.isFile() || stat.isDirectory()), `${relative}/${rel}: unsupported file type`);
    if (stat.isDirectory()) {
      for (const name of (await fs.readdir(full)).sort()) {
        const child = rel ? `${rel}/${name}` : name;
        requireCheck(safePath(child), `${relative}: unsafe file name`);
        await visit(child);
      }
    } else {
      requireCheck(stat.size <= MAX && files.size < 10_000, `${relative}: file limit exceeded`);
      files.set(rel, await fs.readFile(full));
    }
  };
  await visit('');
  return files;
}
// Validate raw tar headers BEFORE invoking tar. No links, extensions or special files.
export function validateTar(archive, prefix) {
  let tar;
  try { tar = gunzipSync(archive, { maxOutputLength: 256 * 1024 * 1024 }); }
  catch { throw new CheckError('archive: invalid or oversized gzip'); }
  const seen = new Set();
  const string = bytes => bytes.toString('utf8').split('\0')[0];
  let offset = 0, longName;
  while (offset + 512 <= tar.length && tar.subarray(offset, offset + 512).some(b => b !== 0)) {
    const header = tar.subarray(offset, offset + 512);
    const number = bytes => {
      const value = string(bytes).trim();
      requireCheck(/^[0-7]+$/u.test(value), 'archive: invalid tar number');
      return parseInt(value, 8);
    };
    const checksum = number(header.subarray(148, 156));
    let sum = 0;
    for (let i = 0; i < 512; i++) sum += i >= 148 && i < 156 ? 32 : header[i];
    requireCheck(sum === checksum, 'archive: invalid tar header checksum');
    const base = string(header.subarray(0, 100));
    const parent = string(header.subarray(345, 500));
    let name = (parent ? `${parent}/${base}` : base).replace(/\/$/u, '');
    const type = header[156];
    const size = number(header.subarray(124, 136));
    requireCheck(size <= MAX && offset + 512 + size <= tar.length, 'archive: entry limit/truncation');
    if (type === 76) {
      requireCheck(base === '././@LongLink' && !parent && longName === undefined && size > 1 && size <= 1024,
        'archive: unsafe long name header');
      const data = tar.subarray(offset + 512, offset + 512 + size);
      requireCheck(data.at(-1) === 0 && !data.subarray(0, -1).includes(0), 'archive: invalid long name');
      longName = data.subarray(0, -1).toString('utf8');
      requireCheck(safePath(longName) && longName.startsWith(`${prefix}/`), 'archive: unsafe long name');
      offset += 512 + Math.ceil(size / 512) * 512;
      continue;
    }
    if (longName !== undefined) { name = longName; longName = undefined; }
    requireCheck(safePath(name) && (name === prefix || name.startsWith(`${prefix}/`))
      && !seen.has(name) && (type === 0 || type === 48 || type === 53)
      && (type !== 53 || size === 0) && (name !== prefix || type === 53), 'archive: unsafe entry');
    seen.add(name);
    requireCheck(seen.size <= 10_000 && size <= MAX && offset + 512 + size <= tar.length, 'archive: entry limit/truncation');
    offset += 512 + Math.ceil(size / 512) * 512;
  }
  requireCheck(longName === undefined && seen.size > 0 && tar.length >= offset + 1024 && tar.subarray(offset).every(b => b === 0), 'archive: invalid tar ending');
  return seen;
}
async function fetchBytes(url, fetchImpl) {
  const controller = new AbortController();
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => { controller.abort(); reject(new CheckError('registry: timeout', 2)); }, 15_000);
  });
  try {
    return await Promise.race([(async () => {
      const response = await fetchImpl(url, { signal: controller.signal });
      if (!response.ok) throw new CheckError('registry: HTTP failure', 2);
      const reader = response.body.getReader();
      const chunks = []; let size = 0;
      try {
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          size += value.length;
          if (size > MAX) throw new CheckError('registry: response too large', 2);
          chunks.push(Buffer.from(value));
        }
      } finally { await reader.cancel(); }
      return Buffer.concat(chunks);
    })(), timeout]);
  } catch (error) {
    if (error instanceof CheckError) throw error;
    throw new CheckError('registry: network failure', 2);
  } finally { clearTimeout(timer); controller.abort(); }
}
function registryRecord(bytes, crate) {
  let records;
  try { records = bytes.toString('utf8').trim().split('\n').map(line => JSON.parse(line)); }
  catch { throw new CheckError(`${crate.name}: invalid registry record`); }
  const exact = records.filter(record => record.name === crate.name && record.vers === crate.version);
  requireCheck(exact.length === 1 && exact[0].cksum === crate.sha256, `${crate.name}: registry checksum/version mismatch`);
  return Buffer.from(JSON.stringify(exact[0]));
}
async function cached(file) {
  try { return await fs.readFile(file); }
  catch (error) { if (error.code === 'ENOENT') return null; throw new CheckError('cache: read failure', 2); }
}
async function atomic(file, bytes) {
  const scratch = `${file}.${randomUUID()}.tmp`;
  try { await fs.writeFile(scratch, bytes, { flag: 'wx' }); await fs.rename(scratch, file); }
  catch { throw new CheckError('cache: write failure', 2); }
  finally { await fs.rm(scratch, { force: true }); }
}
export async function verifiedArchive(crate, cache, fetchImpl) {
  const key = `${crate.name}-${crate.version}-${crate.sha256}`;
  const recordFile = path.join(cache, `${key}.json`), archiveFile = path.join(cache, `${key}.crate`);
  const savedRecord = await cached(recordFile), savedArchive = await cached(archiveFile);
  let validRecord = false;
  if (savedRecord) { try { registryRecord(savedRecord, crate); validRecord = true; } catch { /* Refetch poisoned record. */ } }
  if (validRecord && savedArchive && sha256(savedArchive) === crate.sha256) return { bytes: savedArchive, warm: true };
  const source = crate.checksumSources.find(s => s.kind === 'registry-exact-version');
  const record = registryRecord(await fetchBytes(source.url, fetchImpl), crate);
  const bytes = await fetchBytes(crate.archiveUrl, fetchImpl);
  requireCheck(sha256(bytes) === crate.sha256, `${crate.name}: archive checksum mismatch`);
  await atomic(recordFile, record); await atomic(archiveFile, bytes);
  return { bytes, warm: false };
}
function manifestFields(text, section) {
  const body = text.split(new RegExp(`^\\[${section.replaceAll('.', '\\.')}\\]\\s*$`, 'm'))[1]?.split(/^\[/mu)[0];
  requireCheck(body !== undefined, `Cargo.toml: missing ${section}`);
  return new Map([...body.matchAll(/^([\w-]+)\s*=\s*(.*)$/gmu)].map(m => [m[1], m[2].trim()]));
}
export async function main({ root = ROOT, cache = path.join(os.tmpdir(), 'alef-patches-cache-v1'), fetch: fetchImpl = globalThis.fetch,
  git = gitState, stdout = console.log, stderr = console.error,
  cleanup = directory => fs.rm(directory, { recursive: true, force: true }) } = {}) {
  let extraction, success;
  let code = 0;
  try {
    const doc = readInventory((await read(root, 'docs/PATCHES.md')).toString('utf8'));
    const state = git(root);
    const patches = manifestFields((await read(root, 'backend/Cargo.toml')).toString('utf8'), 'patch.crates-io');
    requireCheck(patches.size === doc.crates.length, 'backend/Cargo.toml: patch inventory mismatch');
    const locks = new Map([['backend/Cargo.lock', lockPackages((await read(root, 'backend/Cargo.lock')).toString('utf8'))]]);
    for (const file of state.tracked) {
      if (file.endsWith('/Cargo.lock')) locks.set(file, lockPackages((await read(root, file)).toString('utf8')));
    }
    try { await fs.mkdir(cache, { recursive: true }); extraction = await fs.mkdtemp(path.join(os.tmpdir(), 'alef-patches-check-')); }
    catch { throw new CheckError('cache: unavailable', 2); }
    let warm = 0, optional = false;
    for (const crate of doc.crates) {
      requireCheck(patches.get(crate.name)?.match(/^\{\s*path\s*=\s*"([^"\n]+)"\s*\}$/u)?.[1] === `patches/${crate.name}`,
        `backend/Cargo.toml: ${crate.name} patch path mismatch`);
      const fields = manifestFields((await read(root, `${crate.root}/Cargo.toml`)).toString('utf8'), 'package');
      requireCheck(fields.get('name') === `"${crate.name}"` && fields.get('version') === `"${crate.version}"`, `${crate.root}/Cargo.toml: package/version mismatch`);
      validateLocks(crate, locks);
      const local = await walk(path.join(root, crate.root), crate.root);
      const tracked = new Set([...state.tracked].filter(f => f.startsWith(`${crate.root}/`)).map(f => f.slice(crate.root.length + 1)));
      for (const file of tracked) requireCheck(local.has(file), `${crate.root}/${file}: tracked file missing on disk`);
      const inventoriedTracked = new Set(crate.files.filter(f => f.tracked && !f.ignored).map(f => f.path));
      for (const file of local.keys()) {
        // Defer listed additions/changes to compareFiles for status, tracking and content pins.
        if (tracked.has(file) || inventoriedTracked.has(file)) continue;
        if (crate.name === 'winit' && file === 'Cargo.lock' && state.winitIgnored) { local.delete(file); optional = true; continue; }
        requireCheck(file === '.cargo_vcs_info.json', `${crate.root}/${file}: unexpected untracked file`);
      }
      const archive = await verifiedArchive(crate, cache, fetchImpl);
      warm += Number(archive.warm);
      const prefix = `${crate.name}-${crate.version}`;
      validateTar(archive.bytes, prefix);
      run('tar', ['-xzf', '-'], extraction, archive.bytes);
      const original = await walk(path.join(extraction, prefix), crate.root);
      compareFiles(crate, original, local, tracked);
    }
    success = `patches: OK (${doc.crates.length} crates; ${warm} verified cache, ${doc.crates.length - warm} fetched${optional ? '; ignored winit Cargo.lock' : ''})`;
  } catch (error) {
    stderr(error instanceof CheckError ? error.message : 'patches: filesystem I/O failure');
    code = error instanceof CheckError ? error.code : 2;
  } finally {
    if (extraction) {
      try { await cleanup(extraction); }
      catch {
        stderr('patches: temporary cleanup failed');
        if (code === 0) code = 2;
      }
    }
  }
  if (code === 0) stdout(success);
  return code;
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = await main();
