// SPDX-License-Identifier: MIT OR Apache-2.0
// The runtime binary of any platform from its npm platform package: registry metadata, the tarball
// checked against the sha512 integrity the registry states, and one finished directory in a cache,
// so a repeated request needs no network. Nothing is executed and no npm client is involved.
import { createHash, randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { chmod, lstat, mkdir, rename, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import zlib from 'node:zlib';
import { keyHome } from '../../keys.mjs';
import { parseVersion } from './bundle-manifest.mjs';

const MIB = 1024 * 1024;
const REGISTRY = 'https://registry.npmjs.org';
const SCOPE = '@alef-runtime';
const METADATA_LIMIT = 16 * MIB;
const TARBALL_LIMIT = 256 * MIB;
const BINARY_LIMIT = 1024 * MIB;
const METADATA_TIMEOUT_MS = 60 * 1000;
const TARBALL_TIMEOUT_MS = 15 * 60 * 1000;
const LICENSES = ['LICENSE-MIT', 'LICENSE-APACHE'];
const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);
const fail = message => { throw new Error(`Runtime: ${message}`); };

// Target table: file suffix, node platform/arch pair and runtime binary name.
export const PLATFORMS = Object.freeze({
  'win32-x64': Object.freeze({ suffix: '.exe', platform: 'win32', arch: 'x64', binary: 'alef.exe' }),
  'darwin-arm64': Object.freeze({ suffix: '', platform: 'darwin', arch: 'arm64', binary: 'alef' }),
  'darwin-x64': Object.freeze({ suffix: '', platform: 'darwin', arch: 'x64', binary: 'alef' }),
  'linux-x64': Object.freeze({ suffix: '', platform: 'linux', arch: 'x64', binary: 'alef' }),
});

// The tools and the runtime are released with one version; a broken layout must not break bundling.
export function toolVersion() {
  try { return String(JSON.parse(readFileSync(new URL('../../../package.json', import.meta.url), 'utf8')).version ?? '0.0.0-dev'); }
  catch { return '0.0.0-dev'; }
}

// Metadata decides what the tarball must be, so it may only travel over a channel nobody rewrites.
function registryOf(value) {
  let url;
  try { url = new URL(value); } catch { fail(`registry '${value}' is not a URL`); }
  if (url.username !== '' || url.password !== '') fail('registry URL must not carry credentials');
  if (url.protocol !== 'https:' && !(url.protocol === 'http:' && LOOPBACK.has(url.hostname))) {
    fail(`registry ${url.origin} must use https (http is accepted only for localhost)`);
  }
  return url.href.replace(/\/+$/, '');
}

async function download(url, { limit, timeoutMs, what }) {
  let response;
  try { response = await fetch(url, { signal: AbortSignal.timeout(timeoutMs), headers: { accept: 'application/vnd.npm.install-v1+json; q=1.0, application/json; q=0.8, */*; q=0.1' } }); }
  catch (cause) { throw new Error(`Runtime: cannot reach ${what} (${cause.cause?.code ?? cause.message})`, { cause }); }
  if (!response.ok) {
    await response.body?.cancel();
    return { status: response.status };
  }
  const declared = Number(response.headers.get('content-length'));
  if (declared > limit) { await response.body?.cancel(); fail(`${what} is larger than ${limit / MIB} MiB`); }
  const chunks = []; let size = 0;
  try {
    for await (const chunk of response.body) {
      size += chunk.length;
      if (size > limit) fail(`${what} is larger than ${limit / MIB} MiB`);
      chunks.push(chunk);
    }
  } catch (cause) {
    if (cause.message.startsWith('Runtime: ')) throw cause;
    throw new Error(`Runtime: reading ${what} failed (${cause.cause?.code ?? cause.message})`, { cause });
  }
  return { status: response.status, bytes: Buffer.concat(chunks) };
}

// Ustar reader for the few named regular files of an `npm pack` tarball; every header is checksummed.
function readTar(archive, wanted) {
  const text = (header, from, length) => {
    const field = header.subarray(from, from + length);
    const end = field.indexOf(0);
    return field.subarray(0, end < 0 ? length : end).toString('utf8');
  };
  const octal = (header, from, length) => {
    if (header[from] & 0x80) fail('the tarball uses base-256 sizes, which are not supported');
    const digits = text(header, from, length).trim();
    if (!/^[0-7]+$/.test(digits)) fail('the tarball has a damaged header');
    return parseInt(digits, 8);
  };
  const found = new Map();
  let offset = 0;
  while (offset + 512 <= archive.length) {
    const header = archive.subarray(offset, offset + 512);
    if (header.every(byte => byte === 0)) break;
    let sum = 0;
    for (let index = 0; index < 512; index++) sum += index >= 148 && index < 156 ? 0x20 : header[index];
    if (sum !== octal(header, 148, 8)) fail('the tarball has a damaged header (checksum)');
    const prefix = text(header, 345, 155);
    const name = prefix === '' ? text(header, 0, 100) : `${prefix}/${text(header, 0, 100)}`;
    const type = header[156];
    const size = octal(header, 124, 12);
    const start = offset + 512;
    if (start + size > archive.length) fail('the tarball is truncated');
    if ((type === 0x30 || type === 0) && wanted.includes(name)) found.set(name, archive.subarray(start, start + size));
    offset = start + Math.ceil(size / 512) * 512;
  }
  return found;
}

async function regularFile(file, damaged) {
  let stat;
  try { stat = await lstat(file); } catch (error) { if (error.code === 'ENOENT') return false; throw error; }
  if (!stat.isFile()) fail(damaged);
  return true;
}

/**
 * Binary of one platform from `@alef-runtime/runtime-<platform>@<version>`, cached under
 * `<cache>/<version>/<platform>/` next to the license texts. Returns a frozen
 * `{ path, version, platform, cached }`; every error is `Runtime: ...` and nothing half-written stays.
 */
export async function fetchRuntime(options = {}) {
  const platform = options.platform ?? `${process.platform}-${process.arch}`;
  if (!Object.hasOwn(PLATFORMS, platform)) fail(`unknown platform ${String(platform)} (known: ${Object.keys(PLATFORMS).join(', ')})`);
  const entry = PLATFORMS[platform];
  const version = options.version ?? toolVersion();
  let parsed;
  try { parsed = parseVersion(version); } catch (cause) { throw new Error(cause.message.replace(/^Bundle: /, 'Runtime: '), { cause }); }
  if (parsed.build !== null) fail(`version ${version} must not carry build metadata`);
  const registry = registryOf(options.registry ?? REGISTRY);
  const root = path.resolve(options.cache ?? path.join(keyHome(), 'cache', 'runtime'));
  const directory = path.join(root, version, platform);
  const binaryPath = path.join(directory, entry.binary);
  const result = cached => Object.freeze({ path: binaryPath, version, platform, cached });
  const damaged = `cache entry ${directory} is damaged: delete it`;
  if (await regularFile(binaryPath, damaged)) return result(true);

  const name = `${SCOPE}/runtime-${platform}`;
  const published = `${name}@${version} is not published on ${registry}; pass --runtime <binary> or --runtime-version <published version>`;
  const metadata = await download(`${registry}/${SCOPE}%2Fruntime-${platform}`, { limit: METADATA_LIMIT, timeoutMs: METADATA_TIMEOUT_MS, what: `the registry entry of ${name}` });
  if (metadata.status === 404) fail(published);
  if (metadata.bytes === undefined) fail(`the registry answered ${metadata.status} for ${name}`);
  let packument;
  try { packument = JSON.parse(metadata.bytes.toString('utf8')); } catch (cause) { throw new Error(`Runtime: the registry entry of ${name} is not JSON`, { cause }); }
  if (typeof packument?.versions !== 'object' || packument.versions === null || !Object.hasOwn(packument.versions, version)) fail(published);
  const dist = packument.versions[version]?.dist;
  const hashes = typeof dist?.integrity === 'string' ? dist.integrity.split(/\s+/).filter(item => item.startsWith('sha512-')) : [];
  if (hashes.length === 0) fail(`${name}@${version} states no sha512 integrity; refusing an unchecked download`);
  if (typeof dist.tarball !== 'string') fail(`${name}@${version} names no tarball`);
  let tarballUrl;
  try { tarballUrl = new URL(dist.tarball); } catch { fail(`${name}@${version} names a tarball that is not a URL`); }
  if (tarballUrl.protocol !== 'https:' && tarballUrl.protocol !== 'http:') fail(`${name}@${version} names a tarball that is not http(s)`);

  const tarball = await download(tarballUrl, { limit: TARBALL_LIMIT, timeoutMs: TARBALL_TIMEOUT_MS, what: `the tarball of ${name}@${version}` });
  if (tarball.bytes === undefined) fail(`the registry answered ${tarball.status} for the tarball of ${name}@${version}`);
  const integrity = `sha512-${createHash('sha512').update(tarball.bytes).digest('base64')}`;
  if (!hashes.includes(integrity)) fail(`the tarball of ${name}@${version} does not match the integrity the registry states`);
  let archive;
  try { archive = zlib.gunzipSync(tarball.bytes, { maxOutputLength: BINARY_LIMIT + 8 * MIB }); }
  catch (cause) { throw new Error(`Runtime: the tarball of ${name}@${version} does not unpack within ${BINARY_LIMIT / MIB} MiB`, { cause }); }
  const files = readTar(archive, ['package/package.json', `package/bin/${entry.binary}`, ...LICENSES.map(license => `package/${license}`)]);
  let manifest;
  try { manifest = JSON.parse(files.get('package/package.json')?.toString('utf8') ?? ''); } catch { fail(`the tarball of ${name}@${version} has no readable package.json`); }
  if (manifest?.name !== name || manifest?.version !== version) fail(`the tarball is ${String(manifest?.name)}@${String(manifest?.version)}, not ${name}@${version}`);
  for (const wanted of [`bin/${entry.binary}`, ...LICENSES]) {
    if (!files.has(`package/${wanted}`)) fail(`the tarball of ${name}@${version} lacks ${wanted}`);
  }
  const binary = files.get(`package/bin/${entry.binary}`);
  if (binary.length === 0 || binary.length > BINARY_LIMIT) fail(`the binary in ${name}@${version} is empty or larger than ${BINARY_LIMIT / MIB} MiB`);

  const parent = path.join(root, version);
  await mkdir(parent, { recursive: true });
  const stage = path.join(parent, `.stage-${randomUUID()}`);
  await mkdir(stage);
  try {
    await writeFile(path.join(stage, entry.binary), binary, { flag: 'wx' });
    if (entry.platform !== 'win32') await chmod(path.join(stage, entry.binary), 0o755);
    for (const license of LICENSES) await writeFile(path.join(stage, license), files.get(`package/${license}`), { flag: 'wx' });
    try { await rename(stage, directory); }
    catch (error) {
      // Another process published the same directory first: its finished copy is as good as ours.
      if (!(await regularFile(binaryPath, damaged))) throw error;
    }
  } finally { await rm(stage, { recursive: true, force: true }); }
  return result(false);
}
