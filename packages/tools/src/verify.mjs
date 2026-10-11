// SPDX-License-Identifier: MIT OR Apache-2.0
import { createHash, createPublicKey, verify as cryptoVerify } from 'node:crypto';
import { readZip } from './zip.mjs';

const HASHES = 'META-INF/hashes';
const SIGNATURE = 'META-INF/signature';
const PREFIX = Buffer.from('alef-package-v1\n');
const UTF8 = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const fail = (name, message) => { throw new Error(`${name}: ${message}`); };
const check = (condition, name, message) => { if (!condition) fail(name, message); };
function text(bytes, name) {
  let value;
  try { value = UTF8.decode(bytes); } catch { fail(name, 'invalid UTF-8'); }
  check(!value.includes('\ufeff'), name, 'BOM is forbidden');
  return value;
}

// Edwards25519 over BigInt: a key or a commitment R of small order (8P is the identity) is refused
// like verify_strict of the runtime refuses it; non-canonical encodings of such points are included.
const P = 2n ** 255n - 19n;
const mod = value => ((value % P) + P) % P;
function power(base, exponent) {
  let result = 1n; base = mod(base);
  for (; exponent > 0n; exponent >>= 1n) { if (exponent & 1n) result = mod(result * base); base = mod(base * base); }
  return result;
}
const invert = value => power(value, P - 2n);
const D = mod(-121665n * invert(121666n));
function decodePoint(bytes) {
  let y = 0n;
  for (let index = 31; index >= 0; index--) y = (y << 8n) | BigInt(bytes[index]);
  const sign = y >> 255n; y = mod(y & ((1n << 255n) - 1n));
  const square = mod(y * y); const u = mod(square - 1n); const v = mod(D * square + 1n);
  let x = power(mod(u * invert(v)), (P + 3n) / 8n);
  if (mod(x * x * v - u) !== 0n) x = mod(x * power(2n, (P - 1n) / 4n));
  if (mod(x * x * v - u) !== 0n) return undefined;
  return [(x & 1n) === sign ? x : mod(-x), y];
}
function doublePoint([x, y]) {
  const t = mod(D * x * x * y * y);
  return [mod(2n * x * y * invert(1n + t)), mod((y * y + x * x) * invert(1n - t))];
}
/** True for a point of order 1, 2, 4 or 8, and for bytes that are no point of the curve. */
export function hasSmallOrder(bytes) {
  let point = decodePoint(bytes);
  if (point === undefined) return true;
  for (let step = 0; step < 3; step++) point = doublePoint(point);
  return point[0] === 0n && point[1] === 1n;
}

function base64(value, length) {
  check(/^[A-Za-z0-9+/]+={1,2}$/.test(value), SIGNATURE, 'noncanonical base64');
  const bytes = Buffer.from(value, 'base64');
  check(bytes.length === length, SIGNATURE, `expected ${length} bytes`);
  check(bytes.toString('base64') === value, SIGNATURE, 'noncanonical base64 pad bits');
  return bytes;
}
function signatureInfo(bytes, hashes) {
  const lines = text(bytes, SIGNATURE).split('\n');
  check(lines.length === 4 && lines[3] === '' && lines[0] === 'alef-package 1' && lines[1].startsWith('key ') && lines[2].startsWith('sig '), SIGNATURE, 'expected exactly three canonical LF lines');
  const key = base64(lines[1].slice(4), 32); const signature = base64(lines[2].slice(4), 64);
  check(!hasSmallOrder(key), SIGNATURE, 'public key of small order');
  check(!hasSmallOrder(signature.subarray(0, 32)), SIGNATURE, 'commitment of small order');
  // Developer convenience only: Rust runtime verify_strict is normative.
  const publicKey = createPublicKey({ key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), key]), format: 'der', type: 'spki' });
  check(cryptoVerify(null, Buffer.concat([PREFIX, hashes]), publicKey, signature), SIGNATURE, 'invalid Ed25519 signature');
  // Typed arrays cannot be frozen. A frozen raw-byte array exposes immutable key
  // material (Buffer.from(info.key) recovers the raw32 public key).
  return { status: 'Signed', key: Object.freeze([...key]), fingerprint: sha256(key) };
}
function parseHashes(bytes, assets) {
  const value = text(bytes, HASHES);
  check(!value.includes('\r'), HASHES, 'CR is forbidden');
  check(value.endsWith('\n'), HASHES, 'missing final LF');
  // Bound the line count before allocating a split array or the hash map.
  let lines = 0;
  for (let at = 0; at < value.length; at++) if (value.charCodeAt(at) === 10) {
    lines++;
    check(lines <= 10_000, HASHES, 'line count limit');
  }
  check(!value.startsWith('\n') && !value.includes('\n\n'), HASHES, 'empty line');
  const expected = new Map(); let previous;
  for (const line of value.slice(0, -1).split('\n')) {
    const match = /^([0-9a-f]{64})  ([A-Za-z0-9._@+/-]+)$/u.exec(line);
    check(match !== null, HASHES, 'noncanonical hash line');
    const [, hash, name] = match; const encoded = Buffer.from(name);
    check(assets.has(name), HASHES, `additional or incorrectly cased entry ${JSON.stringify(name)}`);
    check(!expected.has(name), HASHES, `duplicate entry ${JSON.stringify(name)}`);
    check(previous === undefined || Buffer.compare(previous, encoded) < 0, HASHES, `out-of-order entry ${JSON.stringify(name)}`);
    expected.set(name, hash); previous = encoded;
  }
  for (const name of assets) check(expected.has(name), HASHES, `missing entry ${JSON.stringify(name)}`);
  return expected;
}

/** Open and fully verify a package path/URL or Uint8Array snapshot.
 * status is Signed or Unsigned; Signed includes immutable raw32 key/fingerprint.
 * entries and read include metadata files too, and all exposed reads rehash.
 * Without a hashes list, opening hashes become reread integrity baselines only;
 * this does not establish authenticity and the result remains Unsigned.
 */
export async function verify(input) {
  const archive = await readZip(input);
  const names = new Set(archive.entries.map(entry => entry.name));
  check(names.has('alef.ktav'), 'alef.ktav', 'required exact root file is missing');
  for (const name of names) {
    check(name.toLowerCase() !== 'alef.ktav' || name === 'alef.ktav', name, 'noncanonical reserved path');
    if (name.toLowerCase() === 'meta-inf' || name.toLowerCase().startsWith('meta-inf/')) {
      check(name === HASHES || name === SIGNATURE, name, 'unexpected or noncanonical META-INF entry');
    }
  }
  check(!names.has(SIGNATURE) || names.has(HASHES), SIGNATURE, `missing ${HASHES}`);
  const assets = new Set([...names].filter(name => name !== HASHES && name !== SIGNATURE));
  const hashes = names.has(HASHES) ? await archive.read(HASHES) : undefined;
  const expected = hashes === undefined ? new Map() : parseHashes(hashes, assets);
  const signature = names.has(SIGNATURE) ? await archive.read(SIGNATURE) : undefined;
  const info = signature === undefined ? { status: 'Unsigned' } : signatureInfo(signature, hashes);
  if (hashes !== undefined) expected.set(HASHES, sha256(hashes));
  if (signature !== undefined) expected.set(SIGNATURE, sha256(signature));
  const baseline = new Map();
  for (const entry of archive.entries) {
    const actual = sha256(await archive.read(entry.name));
    if (expected.has(entry.name)) check(actual === expected.get(entry.name), entry.name, `SHA-256 mismatch against ${HASHES}`);
    baseline.set(entry.name, actual);
  }
  async function read(name) {
    check(baseline.has(name), String(name), 'unknown file');
    let bytes;
    try { bytes = await archive.read(name); } catch (error) { throw new Error(`${name}: ${error.message}`, { cause: error }); }
    check(sha256(bytes) === baseline.get(name), name, 'SHA-256 mismatch on reread');
    return bytes;
  }
  const entries = Object.freeze(archive.entries.map(entry => Object.freeze({ ...entry, read: () => read(entry.name) })));
  return Object.freeze({ ...info, entries, read });
}
