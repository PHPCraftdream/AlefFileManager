// SPDX-License-Identifier: MIT OR Apache-2.0
import { deflateSync } from 'node:zlib';

// Embedded 1x1 RGBA PNG pixels; calculate each chunk's CRC instead of relying on opaque bytes.
function chunk(type, data) {
  const label = Buffer.from(type);
  let crc = 0xffffffff;
  for (const byte of Buffer.concat([label, data])) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
  }
  const length = Buffer.alloc(4); length.writeUInt32BE(data.length);
  const checksum = Buffer.alloc(4); checksum.writeUInt32BE((crc ^ 0xffffffff) >>> 0);
  return Buffer.concat([length, label, data, checksum]);
}
export function iconPng() {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(1, 0); header.writeUInt32BE(1, 4);
  header[8] = 8; header[9] = 6;
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', header),
    chunk('IDAT', deflateSync(Buffer.from([0, 70, 100, 220, 255]))),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}
