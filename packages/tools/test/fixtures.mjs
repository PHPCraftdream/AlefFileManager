// SPDX-License-Identifier: MIT OR Apache-2.0
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { deflateRawSync, crc32, gzipSync } from 'node:zlib';

// Independent oracle for the ustar tarball of an `npm pack`: entries are `{ name, data }`, names up to 100 bytes.
export function tarball(entries) {
  const blocks = [];
  for (const { name, data } of entries) {
    const header = Buffer.alloc(512);
    header.write(name, 0, 100, 'utf8');
    header.write('0000755\0', 100, 'ascii');
    header.write('0000000\0', 108, 'ascii'); header.write('0000000\0', 116, 'ascii');
    header.write(`${data.length.toString(8).padStart(11, '0')}\0`, 124, 'ascii');
    header.write('00000000000\0', 136, 'ascii');
    header.write('        ', 148, 'ascii');
    header.write('0', 156, 'ascii');
    header.write('ustar\0', 257, 'ascii'); header.write('00', 263, 'ascii');
    let sum = 0;
    for (const byte of header) sum += byte;
    header.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 'ascii');
    blocks.push(header, data, Buffer.alloc((512 - (data.length % 512)) % 512));
  }
  blocks.push(Buffer.alloc(1024));
  return gzipSync(Buffer.concat(blocks));
}

// The tarball of a platform package as make-platform-package writes it; `manifest` overrides the fields.
export function platformPackage(id, version, binary, manifest = {}) {
  return tarball([
    { name: 'package/package.json', data: Buffer.from(JSON.stringify({ name: `@alef-runtime/runtime-${id}`, version, ...manifest })) },
    { name: `package/bin/${id.startsWith('win32') ? 'alef.exe' : 'alef'}`, data: binary },
    { name: 'package/LICENSE-MIT', data: Buffer.from('MIT text\n') },
    { name: 'package/LICENSE-APACHE', data: Buffer.from('Apache text\n') },
  ]);
}

// A loopback npm registry. `packages` is `{ [platform]: { [version]: { tarball, integrity? } } }`;
// `requests` lists every URL that was asked for.
export async function registry(packages) {
  const requests = [];
  let url = '';
  const server = createServer((request, response) => {
    requests.push(request.url);
    const reply = (status, body, type = 'application/json') => { response.writeHead(status, { 'content-type': type }); response.end(body); };
    const entry = /^\/@alef-runtime%2Fruntime-([a-z0-9-]+)$/.exec(request.url);
    const file = /^\/tarballs\/([a-z0-9-]+)\/([0-9A-Za-z.-]+)\.tgz$/.exec(request.url);
    if (entry && Object.hasOwn(packages, entry[1])) {
      const versions = {};
      for (const [version, item] of Object.entries(packages[entry[1]])) {
        versions[version] = {
          name: `@alef-runtime/runtime-${entry[1]}`, version,
          dist: { tarball: `${url}/tarballs/${entry[1]}/${version}.tgz`, integrity: item.integrity ?? `sha512-${createHash('sha512').update(item.tarball).digest('base64')}` },
        };
      }
      reply(200, JSON.stringify({ name: `@alef-runtime/runtime-${entry[1]}`, versions }));
    } else if (file && Object.hasOwn(packages, file[1]) && Object.hasOwn(packages[file[1]], file[2])) {
      reply(200, packages[file[1]][file[2]].tarball, 'application/octet-stream');
    } else reply(404, '{"error":"not found"}');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  url = `http://127.0.0.1:${server.address().port}`;
  return { url, requests, close: () => new Promise(resolve => { server.close(resolve); server.closeAllConnections(); }) };
}

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
