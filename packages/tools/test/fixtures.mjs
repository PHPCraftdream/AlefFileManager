// SPDX-License-Identifier: MIT OR Apache-2.0
import { deflateRawSync, crc32 } from 'node:zlib';

// Independent wire-format oracle: no production writer or name validator.
// Deliberately permits invalid fields, arbitrary bytes, gaps and central order.
export function handmade(entries = [], options = {}) {
  const locals = [options.prefix ?? Buffer.alloc(0)]; const centrals = [];
  let offset = locals[0].length;
  for (const entry of entries) {
    const name = Buffer.isBuffer(entry.name) ? entry.name : Buffer.from(entry.name);
    const data = entry.data ?? Buffer.alloc(0);
    const method = entry.method ?? 0;
    const raw = entry.raw ?? (method === 8 ? deflateRawSync(data) : data);
    const localName = entry.localName ?? name;
    const localExtra = entry.localExtra ?? Buffer.alloc(0);
    const centralExtra = entry.centralExtra ?? Buffer.alloc(0);
    const comment = entry.comment ?? Buffer.alloc(0);
    const local = Buffer.alloc(30); const central = Buffer.alloc(46);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(entry.version ?? 20, 4);
    local.writeUInt16LE(entry.flags ?? 0x0800, 6);
    local.writeUInt16LE(method, 8);
    local.writeUInt16LE(0x21, 12);
    local.writeUInt32LE(entry.crc ?? crc32(data), 14);
    local.writeUInt32LE(entry.compressedSize ?? raw.length, 18);
    local.writeUInt32LE(entry.size ?? data.length, 22);
    local.writeUInt16LE(localName.length, 26);
    local.writeUInt16LE(localExtra.length, 28);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(0x031e, 4);
    local.copy(central, 6, 4, 26);
    central.writeUInt16LE(name.length, 28);
    central.writeUInt16LE(centralExtra.length, 30);
    central.writeUInt16LE(comment.length, 32);
    central.writeUInt32LE(entry.attrs ?? 0x81a40000, 38);
    central.writeUInt32LE(entry.offset ?? offset, 42);
    const pieces = [local, localName, localExtra, raw, entry.after ?? Buffer.alloc(0)];
    locals.push(...pieces); offset += pieces.reduce((sum, piece) => sum + piece.length, 0);
    centrals.push(Buffer.concat([central, name, centralExtra, comment]));
  }
  const order = options.order ?? centrals.map((_, index) => index);
  const directory = Buffer.concat(order.map(index => centrals[index]));
  const end = Buffer.alloc(22); end.writeUInt32LE(0x06054b50);
  end.writeUInt16LE(order.length, 8); end.writeUInt16LE(order.length, 10);
  end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16);
  end.writeUInt16LE(options.endComment?.length ?? 0, 20);
  return Buffer.concat([...locals, directory, end, options.endComment ?? Buffer.alloc(0), options.suffix ?? Buffer.alloc(0)]);
}
