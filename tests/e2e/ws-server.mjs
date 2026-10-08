// SPDX-License-Identifier: MIT OR Apache-2.0
// A small server of WebSocket (RFC 6455) for the end-to-end scenarios, on the loopback: it echoes what it
// gets, and by the path of the request it says more. `/bye` sends a word and closes with 4001; `/big` sends
// 4 MiB first. It chooses the subprotocol `superchat` for whoever offers it. `seen` counts what it was asked.
import { createHash } from 'node:crypto';
import http from 'node:http';
import https from 'node:https';

const GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';
export const BIG = 4 * 1024 * 1024;

/** The byte at a place of `/big`, the same pattern the page works out again. */
export const bigByte = at => (at * 31 + (at >> 8)) & 255;

function frame(opcode, payload) {
  const length = payload.length;
  const head = length < 126 ? Buffer.from([0x80 | opcode, length])
    : length < 65536 ? Buffer.from([0x80 | opcode, 126, length >> 8, length & 255])
      : Buffer.concat([Buffer.from([0x80 | opcode, 127]), (() => { const eight = Buffer.alloc(8); eight.writeBigUInt64BE(BigInt(length)); return eight; })()]);
  return Buffer.concat([head, payload]);
}

/** The first whole frame of a buffer of the bytes of a client (masked), or `null` while it is not whole. */
function parse(buffer) {
  if (buffer.length < 2) return null;
  const opcode = buffer[0] & 15;
  const masked = (buffer[1] & 128) !== 0;
  let length = buffer[1] & 127;
  let at = 2;
  if (length === 126) {
    if (buffer.length < 4) return null;
    length = buffer.readUInt16BE(2);
    at = 4;
  } else if (length === 127) {
    if (buffer.length < 10) return null;
    length = Number(buffer.readBigUInt64BE(2));
    at = 10;
  }
  const maskAt = at;
  if (masked) at += 4;
  if (buffer.length < at + length) return null;
  const payload = Buffer.from(buffer.subarray(at, at + length));
  if (masked) for (let i = 0; i < length; i += 1) payload[i] ^= buffer[maskAt + (i & 3)];
  return { opcode, fin: (buffer[0] & 128) !== 0, payload, size: at + length };
}

function onUpgrade(seen, sockets) {
  return (request, socket) => {
    seen.upgrades += 1;
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
    socket.on('error', () => {});
    const offered = (request.headers['sec-websocket-protocol'] ?? '').split(',').map(name => name.trim());
    const chosen = offered.includes('superchat') ? 'superchat' : null;
    const accept = createHash('sha1').update(request.headers['sec-websocket-key'] + GUID).digest('base64');
    socket.write([
      'HTTP/1.1 101 Switching Protocols', 'Upgrade: websocket', 'Connection: Upgrade', `Sec-WebSocket-Accept: ${accept}`,
      ...(chosen ? [`Sec-WebSocket-Protocol: ${chosen}`] : []), '', '',
    ].join('\r\n'));
    seen.headers.push({ url: request.url, origin: request.headers.origin ?? null, protocol: chosen });

    if (request.url === '/bye') {
      socket.write(frame(1, Buffer.from('last')));
      socket.write(frame(8, Buffer.concat([Buffer.from([4001 >> 8, 4001 & 255]), Buffer.from('bye')])));
    } else if (request.url === '/big') {
      const data = Buffer.allocUnsafe(BIG);
      for (let i = 0; i < BIG; i += 1) data[i] = bigByte(i);
      socket.write(frame(2, data));
    }

    let buffer = Buffer.alloc(0);
    let pieces = [];
    let kind = 0;
    socket.on('data', chunk => {
      buffer = Buffer.concat([buffer, chunk]);
      for (let next = parse(buffer); next; next = parse(buffer)) {
        buffer = buffer.subarray(next.size);
        const { opcode, fin, payload } = next;
        if (opcode === 8) {
          seen.closes.push({ code: payload.length >= 2 ? payload.readUInt16BE(0) : 1005, reason: payload.subarray(2).toString() });
          socket.write(frame(8, payload));
          socket.end();
        } else if (opcode === 9) {
          socket.write(frame(10, payload));
        } else if (opcode === 1 || opcode === 2 || opcode === 0) {
          if (opcode !== 0) kind = opcode;
          pieces.push(payload);
          if (fin) {
            seen.messages += 1;
            socket.write(frame(kind, Buffer.concat(pieces)));
            pieces = [];
          }
        }
      }
    });
  };
}

/** A server of WebSocket on a port of the loopback; `tls` (`{ key, cert }`) makes it a secure one. */
export function wsServer({ tls } = {}) {
  const seen = { upgrades: 0, messages: 0, closes: [], headers: [] };
  const sockets = new Set();
  const answer = (_, response) => { response.writeHead(426); response.end(); };
  const server = tls ? https.createServer(tls, answer) : http.createServer(answer);
  server.on('upgrade', onUpgrade(seen, sockets));
  server.on('tlsClientError', () => {});
  return new Promise(resolve => {
    server.listen(0, '127.0.0.1', () => resolve({
      port: server.address().port, seen,
      close: () => new Promise(done => {
        for (const socket of sockets) socket.destroy();
        server.close(done);
      }),
    }));
  });
}
