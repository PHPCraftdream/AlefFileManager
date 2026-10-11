// SPDX-License-Identifier: MIT OR Apache-2.0
import { open } from 'node:fs/promises';
import { createInflateRaw, deflateRawSync, crc32 } from 'node:zlib';
import { Writable } from 'node:stream';
import { pipeline } from 'node:stream/promises';

const MAX_ENTRIES = 10_000;
const MAX_FILE = 512 * 1024 * 1024;
const MAX_TOTAL = 2 * 1024 * 1024 * 1024;
const MAX_ARCHIVE = 2304 * 1024 * 1024;
const MAX_CENTRAL = 16 * 1024 * 1024;
const fail = message => { throw new Error(`ZIP: ${message}`); };
const check = (condition, message) => { if (!condition) fail(message); };
/** Validate a strict ASCII file name, without normalization. */
export function validateName(name) {
  check(typeof name === 'string', 'name must be a string');
  check(name.length > 0 && /^[A-Za-z0-9._@+/-]+$/.test(name), 'empty or non-ASCII / forbidden name character');
  check(!name.startsWith('/'), 'absolute name');
  check(name.length <= 1024, 'name exceeds 1024 bytes');
  const parts = name.split('/');
  for (const part of parts) {
    check(part !== '' && part !== '.' && part !== '..', 'empty or traversal name segment');
    check(!/[ .]$/.test(part), 'name segment ends in space or dot');
    check(!/^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\.|$)/i.test(part), 'Windows device name');
    check(Buffer.byteLength(part) <= 255, 'segment exceeds 255 UTF-8 bytes');
  }
  const key = name.toLowerCase();
  const reserved = ['alef.ktav', 'META-INF/hashes', 'META-INF/signature'];
  for (const exact of reserved) check(key !== exact.toLowerCase() || name === exact, 'reserved name spelling');
  check(!key.startsWith('meta-inf/') || reserved.includes(name), 'META-INF allowlist');
  return name;
}
function registerName(names, name) {
  validateName(name);
  const key = name.toLowerCase();
  check(!names.files.has(key), 'case-insensitive duplicate name');
  check(!names.prefixes.has(key), 'file path-prefix conflict');
  const segments = key.split('/');
  for (let i = 1; i < segments.length; i++) {
    const prefix = segments.slice(0, i).join('/');
    check(!names.files.has(prefix), 'file path-prefix conflict');
    names.prefixes.add(prefix);
  }
  names.files.add(key);
}
function nameRegistry() { return { files: new Set(), prefixes: new Set() }; }
function decodeName(bytes) {
  check(bytes.every(byte => byte < 128), 'non-ASCII name');
  return validateName(bytes.toString('ascii'));
}
function add(at, length, bound, message = 'bounds / overflow') {
  check(Number.isSafeInteger(at) && Number.isSafeInteger(length) && at >= 0 && length >= 0 && at <= bound && length <= bound - at, message);
  return at + length;
}
function sizeLimit(name, size) {
  check(size <= MAX_FILE, 'file size limit');
  if (name === 'META-INF/hashes') check(size <= MAX_CENTRAL, 'hashes size limit');
  if (name === 'META-INF/signature') check(size <= 256, 'signature size limit');
}
function fields(header, central) {
  const shift = central ? 2 : 0;
  const version = header.readUInt16LE(4 + shift);
  const flags = header.readUInt16LE(6 + shift);
  const method = header.readUInt16LE(8 + shift);
  check(version <= 20, 'unsupported version / ZIP64');
  check(method === 0 || method === 8, 'unsupported compression method');
  check(flags === 0x0800, 'flags must equal 0x0800');
  const compressedSize = header.readUInt32LE(18 + shift);
  const size = header.readUInt32LE(22 + shift);
  check(compressedSize !== 0xffffffff && size !== 0xffffffff, 'ZIP64 size');
  check(size <= MAX_FILE, 'file size limit');
  check(method !== 0 || compressedSize === size, 'stored size disagreement');
  return { version, flags, method, time: header.readUInt16LE(10 + shift), date: header.readUInt16LE(12 + shift), crc: header.readUInt32LE(14 + shift), compressedSize, size };
}

/**
 * Return deterministic ZIP bytes. Entries are iterable { name, data, method? };
 * data is a Uint8Array. By default use deflate only when smaller, otherwise stored.
 * Explicit entry.method or options.method (0 or 8) is a ZIP testing API;
 * package packing must use automatic selection.
 * No package composition/signature policy is imposed by this ZIP layer.
 */
export function writeZip(entries, options = {}) {
  const names = nameRegistry(); const records = []; let total = 0;
  const defaultMethod = options.method;
  check(defaultMethod === undefined || defaultMethod === 0 || defaultMethod === 8, 'unsupported compression method');
  for (const entry of entries) {
    check(records.length < MAX_ENTRIES, 'entry count limit');
    registerName(names, entry.name);
    check(entry.data instanceof Uint8Array, 'entry data must be bytes');
    sizeLimit(entry.name, entry.data.byteLength);
    total = add(total, entry.data.byteLength, MAX_TOTAL, 'total size limit');
    const data = Buffer.from(entry.data);
    const method = entry.method ?? defaultMethod;
    check(method === undefined || method === 0 || method === 8, 'unsupported compression method');
    records.push({ name: Buffer.from(entry.name), data, method });
  }
  records.sort((a, b) => Buffer.compare(a.name, b.name));
  const locals = []; const centrals = []; let offset = 0; let centralSize = 0;
  for (const record of records) {
    const { name, data } = record;
    let method = record.method;
    let compressed = method === 0 ? data : deflateRawSync(data, { level: 9 });
    if (method === undefined) {
      method = compressed.length < data.length ? 8 : 0;
      if (method === 0) compressed = data;
    }
    const local = Buffer.alloc(30); const central = Buffer.alloc(46);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0x800, 6); local.writeUInt16LE(method, 8);
    local.writeUInt16LE(0x21, 12); local.writeUInt32LE(crc32(data), 14);
    local.writeUInt32LE(compressed.length, 18); local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(name.length, 26);
    central.writeUInt32LE(0x02014b50, 0); central.writeUInt16LE(0x031e, 4);
    local.copy(central, 6, 4, 30);
    central.writeUInt32LE((0o100644 * 65536) >>> 0, 38); central.writeUInt32LE(offset, 42);
    locals.push(local, name, compressed); centrals.push(central, name);
    offset = add(offset, local.length + name.length + compressed.length, MAX_ARCHIVE, 'archive size limit');
    centralSize = add(centralSize, central.length + name.length, MAX_CENTRAL, 'central directory size limit');
    add(offset, centralSize + 22, MAX_ARCHIVE, 'archive size limit');
  }
  const end = Buffer.alloc(22); end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(records.length, 8); end.writeUInt16LE(records.length, 10);
  end.writeUInt32LE(centralSize, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, ...centrals, end]);
}

async function sourceFor(input) {
  if (input instanceof Uint8Array) {
    check(input.byteLength <= MAX_ARCHIVE, 'archive size limit');
    // Preflight limits before allocating the immutable snapshot.
    const bytes = Buffer.from(input.buffer, input.byteOffset, input.byteLength);
    return { size: bytes.length, memory: true, bytes(at, length) {
      add(at, length, bytes.length, 'truncated archive');
      return bytes.subarray(at, at + length);
    }, async close() {} };
  }
  const file = await open(input, 'r');
  try {
    const stat = await file.stat();
    check(stat.isFile() && Number.isSafeInteger(stat.size), 'not a regular file');
    check(stat.size <= MAX_ARCHIVE, 'archive size limit');
    return { size: stat.size, async bytes(at, length) {
      add(at, length, stat.size, 'truncated archive');
      const bytes = Buffer.alloc(length); let done = 0;
      while (done < length) {
        const { bytesRead } = await file.read(bytes, done, length - done, at + done);
        check(bytesRead > 0, 'truncated archive'); done += bytesRead;
      }
      return bytes;
    }, async close() { await file.close(); } };
  } catch (error) { await file.close(); throw error; }
}

async function directory(source) {
  check(source.size >= 22 && source.size <= MAX_ARCHIVE, 'archive size limit');
  const end = await source.bytes(source.size - 22, 22);
  check(end.readUInt32LE(0) === 0x06054b50, 'missing end record or trailing archive bytes');
  check(end.readUInt16LE(20) === 0, 'end record comment');
  const count = end.readUInt16LE(10);
  check(end.readUInt16LE(4) === 0 && end.readUInt16LE(6) === 0 && end.readUInt16LE(8) === count, 'multidisk archive');
  check(count <= MAX_ENTRIES, 'entry count limit');
  const centralSize = end.readUInt32LE(12); const centralAt = end.readUInt32LE(16);
  check(centralAt !== 0xffffffff && centralSize !== 0xffffffff, 'ZIP64 central directory');
  check(centralSize <= MAX_CENTRAL, 'central directory size limit');
  const centralEnd = add(centralAt, centralSize, source.size - 22, 'central directory bounds / ZIP64');
  check(centralEnd === source.size - 22, 'central directory bounds / ZIP64');
  // The EOCD bounds and limit above are checked before this allocation.
  const central = source.memory ? source.bytes(centralAt, centralSize) : await source.bytes(centralAt, centralSize);
  const records = []; const names = nameRegistry(); let at = centralAt; let total = 0;
  for (let index = 0; index < count; index++) {
    check(at + 46 <= centralAt + centralSize, 'truncated central directory');
    const header = central.subarray(at - centralAt, at - centralAt + 46);
    let context = `central entry #${index}`;
    try {
      const contextLength = header.readUInt16LE(28);
      if (contextLength > 0 && contextLength <= 1024 && at + 46 + contextLength <= centralAt + centralSize) {
        context = JSON.stringify(central.subarray(at - centralAt + 46, at - centralAt + 46 + contextLength).toString('utf8'));
      }
      check(header.readUInt32LE(0) === 0x02014b50, 'central signature');
      const record = fields(header, true);
      const nameLength = header.readUInt16LE(28); const extraLength = header.readUInt16LE(30); const commentLength = header.readUInt16LE(32);
      check(nameLength > 0 && nameLength <= 1024, 'name length');
      check(header.readUInt16LE(34) === 0, 'multidisk entry');
      const localAt = header.readUInt32LE(42);
      check(localAt !== 0xffffffff && localAt < centralAt, 'local offset / ZIP64');
      check(extraLength === 0, 'central extra fields forbidden');
      check(commentLength === 0, 'central comments forbidden');
      const next = add(at, 46 + nameLength, centralEnd, 'central entry bounds');
      const nameBytes = central.subarray(at - centralAt + 46, next - centralAt); const name = decodeName(nameBytes);
      registerName(names, name);
      sizeLimit(name, record.size);
      total = add(total, record.size, MAX_TOTAL, 'total size limit');
      records.push({ ...record, name, nameBytes, localAt }); at = next;
    } catch (error) { throw new Error(`ZIP entry ${context}: ${error.message}`, { cause: error }); }
  }
  check(at === centralAt + centralSize, 'central directory size disagreement');
  let localEnd = 0;
  for (const record of records) {
    try {
      check(record.localAt === localEnd, 'local overlap, gap or unreferenced record');
      check(record.localAt + 30 <= centralAt, 'local header bounds');
      const local = source.memory ? source.bytes(record.localAt, 30) : await source.bytes(record.localAt, 30);
      check(local.readUInt32LE(0) === 0x04034b50, 'local signature');
      const values = fields(local, false);
      for (const key of Object.keys(values)) check(values[key] === record[key], `local ${key} disagreement`);
      const nameLength = local.readUInt16LE(26); const extraLength = local.readUInt16LE(28);
      check(nameLength === record.nameBytes.length, 'local name disagreement');
      check(extraLength === 0, 'local extra fields forbidden');
      record.dataAt = add(record.localAt, 30 + nameLength, centralAt, 'local header bounds');
      localEnd = add(record.dataAt, record.compressedSize, centralAt, 'local data bounds / overlap');
      const variable = source.memory ? source.bytes(record.localAt + 30, nameLength) : await source.bytes(record.localAt + 30, nameLength);
      check(variable.equals(record.nameBytes), 'local name disagreement');
    } catch (error) { throw new Error(`ZIP entry ${JSON.stringify(record.name)}: ${error.message}`, { cause: error }); }
  }
  check(localEnd === centralAt, 'unreferenced local data / descriptor');
  return records;
}

function storedContent(source, record, collect) {
  try {
    let crc = 0;
    // Metadata already proves stored compressed and uncompressed sizes agree.
    for (let at = 0; at < record.size; at += 65536) {
      crc = crc32(source.bytes(record.dataAt + at, Math.min(65536, record.size - at)), crc);
    }
    check(crc === record.crc, 'CRC mismatch');
    return collect ? Buffer.from(source.bytes(record.dataAt, record.size)) : undefined;
  } catch (error) { throw new Error(`ZIP entry ${JSON.stringify(record.name)}: ${error.message}`, { cause: error }); }
}

async function content(source, record, collect) {
  if (source.memory && record.method === 0) return storedContent(source, record, collect);
  try { return await contentBytes(source, record, collect); }
  catch (error) { throw new Error(`ZIP entry ${JSON.stringify(record.name)}: ${error.message}`, { cause: error }); }
}
async function contentBytes(source, record, collect) {
  let size = 0; let crc = 0; const chunks = [];
  function accept(bytes) {
    size += bytes.length;
    check(size <= record.size && size <= MAX_FILE, 'inflated size exceeds declared size');
    crc = crc32(bytes, crc); if (collect) chunks.push(bytes);
  }
  async function* compressed() {
    for (let at = 0; at < record.compressedSize; at += 65536) {
      yield await source.bytes(record.dataAt + at, Math.min(65536, record.compressedSize - at));
    }
  }
  if (record.method === 0) {
    for await (const bytes of compressed()) accept(bytes);
  } else {
    // Small native chunks plus the sink's declared + 1 limit bound bomb output.
    const inflate = createInflateRaw({ chunkSize: Math.max(64, Math.min(65536, record.size + 1)), maxOutputLength: record.size + 1 });
    const sink = new Writable({ write(bytes, _encoding, callback) {
      // Native zlib has a minimum 64-byte output chunk. Consume at most the
      // declared remainder + 1, then destroy the pipeline immediately on excess.
      try { accept(bytes.subarray(0, Math.min(bytes.length, record.size - size + 1))); callback(); } catch (error) { callback(error); }
    } });
    await pipeline(compressed(), inflate, sink);
    check(inflate.bytesWritten === record.compressedSize, 'trailing compressed bytes');
  }
  check(size === record.size, 'uncompressed size disagreement');
  check(crc === record.crc, 'CRC mismatch');
  return collect ? Buffer.concat(chunks, size) : undefined;
}

/**
 * Validate limits and structure before allocating an immutable archive snapshot.
 * All validation and subsequent CRC-checked reads consume that same snapshot.
 * Package composition and SHA-256/signature policy belong to the package layer.
 */
export async function readZip(input) {
  const original = await sourceFor(input); let snapshot;
  try {
    await directory(original);
    const bytes = await original.bytes(0, original.size);
    snapshot = input instanceof Uint8Array ? Buffer.from(bytes) : bytes;
  } finally { await original.close(); }
  const source = await sourceFor(snapshot);
  const records = await directory(source);
  for (const record of records) {
    if (record.method === 0) storedContent(source, record, false);
    else await content(source, record, false);
  }
  const byName = new Map(records.map(record => [record.name, record]));
  async function read(name) {
    const record = byName.get(name); check(record !== undefined, `unknown file ${JSON.stringify(name)}`);
    return await content(source, record, true);
  }
  const entries = Object.freeze([...byName.values()].map(record => Object.freeze({
    name: record.name, size: record.size, compressedSize: record.compressedSize, method: record.method,
    crc: record.crc, read: () => read(record.name),
  })));
  return Object.freeze({ entries, read });
}
