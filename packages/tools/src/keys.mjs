// SPDX-License-Identifier: MIT OR Apache-2.0
import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, randomUUID } from 'node:crypto';
import { chmod, lstat, mkdir, open, readdir, link, unlink } from 'node:fs/promises';
import { constants } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { validateName } from './zip.mjs';

export function keyHome(options = {}) {
  return path.resolve(options.home ?? process.env.ALEF_TRON_HOME ?? path.join(homedir(), '.alef-tron'));
}
function keyName(name) {
  validateName(name);
  validateName(`${name}.pem`);
  if (name.includes('/') || !/^[A-Za-z0-9_-][A-Za-z0-9_.-]*$/.test(name)) throw new Error('Invalid key name');
  return name;
}
export async function noSymlinks(target) {
  const absolute = path.resolve(target); const root = path.parse(absolute).root;
  let current = root;
  for (const part of absolute.slice(root.length).split(path.sep).filter(Boolean)) {
    current = path.join(current, part);
    try {
      if ((await lstat(current)).isSymbolicLink()) throw new Error(`Symlink forbidden: ${current}`);
    } catch (error) { if (error.code === 'ENOENT') return; throw error; }
  }
}
function publicInfo(name, file, privateKey) {
  const der = createPublicKey(privateKey).export({ format: 'der', type: 'spki' });
  const raw = der.subarray(-32);
  return Object.freeze({ name, path: file, key: Object.freeze([...raw]), fingerprint: createHash('sha256').update(raw).digest('hex') });
}
async function directory(options, create = false) {
  const home = keyHome(options); const dir = path.join(home, 'keys');
  await noSymlinks(dir);
  if (create) {
    await mkdir(dir, { recursive: true, mode: 0o700 });
    await noSymlinks(dir);
    await chmod(home, 0o700); await chmod(dir, 0o700);
  }
  return dir;
}
export async function createKey(name = 'default', options = {}) {
  keyName(name);
  const dir = await directory(options, true); const file = path.join(dir, `${name}.pem`);
  await noSymlinks(file);
  const { privateKey } = generateKeyPairSync('ed25519');
  const temp = path.join(dir, `.new-${randomUUID()}`);
  const handle = await open(temp, 'wx', 0o600);
  try {
    try {
      await handle.writeFile(privateKey.export({ format: 'pem', type: 'pkcs8' }));
      await handle.sync();
    } finally { await handle.close(); }
    await noSymlinks(file);
    await link(temp, file);
  } finally { await unlink(temp); }
  return publicInfo(name, file, privateKey);
}
export async function loadKey(name = 'default', options = {}) {
  keyName(name);
  const file = path.join(await directory(options), `${name}.pem`);
  await noSymlinks(file);
  const before = await lstat(file);
  if (!before.isFile() || before.size > 16384) throw new Error(`Invalid private key file: ${file}`);
  const handle = await open(file, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > 16384 || stat.dev !== before.dev || stat.ino !== before.ino) throw new Error('Private key file changed');
    const bytes = Buffer.alloc(stat.size + 1);
    let size = 0;
    while (size < bytes.length) {
      const { bytesRead } = await handle.read(bytes, size, bytes.length - size, size);
      if (!bytesRead) break;
      size += bytesRead;
    }
    if (size !== stat.size) throw new Error('Private key file changed');
    const pem = bytes.subarray(0, size).toString('utf8');
    if (!/^-----BEGIN PRIVATE KEY-----\r?\n[A-Za-z0-9+/=\r\n]+-----END PRIVATE KEY-----\r?\n?$/.test(pem)) throw new Error('Expected PKCS8 PEM private key');
    const key = createPrivateKey({ key: pem, format: 'pem', type: 'pkcs8' });
    if (key.type !== 'private' || key.asymmetricKeyType !== 'ed25519') throw new Error('Expected Ed25519 private signing key');
    return key;
  } finally { await handle.close(); }
}
export async function listKeys(options = {}) {
  const dir = await directory(options); let entries;
  try { entries = await readdir(dir); } catch (error) { if (error.code === 'ENOENT') return []; throw error; }
  const result = [];
  for (const entry of entries.sort()) {
    if (!entry.endsWith('.pem')) continue;
    const name = keyName(entry.slice(0, -4));
    result.push(publicInfo(name, path.join(dir, entry), await loadKey(name, options)));
  }
  return result;
}
