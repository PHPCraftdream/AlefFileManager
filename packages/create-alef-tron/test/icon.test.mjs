// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { inflateSync } from 'node:zlib';
import { iconPng } from '../src/icon.mjs';

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = (c & 1) ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

function chunks(png) {
  const list = [];
  let offset = 8;
  while (offset < png.length) {
    const length = png.readUInt32BE(offset);
    const type = png.subarray(offset + 4, offset + 8).toString('ascii');
    const data = png.subarray(offset + 8, offset + 8 + length);
    const crc = png.readUInt32BE(offset + 8 + length);
    list.push({ type, data, crc });
    offset += 12 + length;
  }
  return list;
}

test('icon_png_is_a_valid_png_with_matching_crcs_and_decodable_idat', () => {
  const png = iconPng();
  assert.deepEqual([...png.subarray(0, 8)], [137, 80, 78, 71, 13, 10, 26, 10], 'PNG signature bytes');
  const list = chunks(png);
  assert.deepEqual(list.map(chunk => chunk.type), ['IHDR', 'IDAT', 'IEND']);
  for (const chunk of list) {
    assert.equal(crc32(Buffer.concat([Buffer.from(chunk.type, 'ascii'), chunk.data])), chunk.crc,
      `CRC32 mismatch in ${chunk.type}`);
  }
  const ihdr = list[0].data;
  assert.equal(ihdr.readUInt32BE(0), 1, 'width');
  assert.equal(ihdr.readUInt32BE(4), 1, 'height');
  assert.equal(ihdr[8], 8, 'bit depth');
  assert.equal(ihdr[9], 6, 'RGBA color type');
  const raw = inflateSync(list[1].data);
  assert.deepEqual([...raw], [0, 70, 100, 220, 255], 'one unfiltered RGBA scanline');
});
