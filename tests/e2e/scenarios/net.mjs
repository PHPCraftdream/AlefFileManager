// SPDX-License-Identifier: MIT OR Apache-2.0
// Scenarios of the network modules (docs/stages/m4-net-cli.md, "Приёмка"): `http` and `socket` against
// servers of the runner on the loopback, with the right allowed and with a stand-in the user chose.
import { createHash } from 'node:crypto';
import dgram from 'node:dgram';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import os from 'node:os';
import { dirname, join } from 'node:path';
import tls from 'node:tls';
import { fileURLToPath } from 'node:url';

const SIZE = 8 * 1024 * 1024;

const HTTP_CHECKS = [
  'http-a-request-and-its-answer', 'http-a-big-body-goes-up-and-comes-down-as-a-stream',
  'http-redirects-follow-and-the-scope-holds-for-every-hop', 'http-time-and-the-network-fail-with-their-own-codes',
  'http-an-abort-reaches-the-connection', 'http-a-download-fills-a-file-with-progress-and-an-abort-leaves-none',
];
const HTTP_SUBSTITUTE_CHECKS = ['http-a-substituted-network-hangs-until-its-time-is-up'];
const SOCKET_CHECKS = [
  'socket-tcp-carries-a-big-body-both-ways-through-an-echo-server', 'socket-a-listener-takes-a-connection-of-the-page-itself',
  'socket-tls-trusts-the-authority-the-page-names-and-no-other', 'socket-udp-goes-between-two-sockets-and-to-a-server',
  'socket-the-scope-holds-for-every-command-and-a-closed-port-is-the-network',
  'socket-a-closed-socket-ends-its-streams-and-the-server-sees-the-end',
];
const SOCKET_SUBSTITUTE_CHECKS = ['socket-a-substituted-network-is-dead-and-takes-no-port'];
/** The certificates of the tests of the runtime: an authority and a server for localhost and the loopback. */
const TLS_FILES = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'backend', 'crates', 'alef-modules', 'tests', 'fixtures', 'tls');

/** The byte at a place of `/big`, the same pattern the page works out again. */
const bigByte = at => (at * 31 + (at >> 8)) & 255;

/** A server on the loopback that answers by the path and remembers what it was sent and what was cut off. */
function origin() {
  const seen = [];
  const cut = [];
  const server = http.createServer((request, response) => {
    const url = new URL(request.url, 'http://127.0.0.1');
    seen.push({ method: request.method, path: url.pathname });
    response.on('close', () => {
      if (!response.writableFinished) cut.push(url.pathname);
    });
    switch (url.pathname) {
      case '/hello':
        response.writeHead(200, { 'content-type': 'text/plain', 'x-test': 'a' });
        response.end('hello');
        return;
      case '/json':
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(JSON.stringify({ ok: true, list: [1, 2, 3] }));
        return;
      case '/nobody':
        response.writeHead(204);
        response.end();
        return;
      case '/notfound':
        response.writeHead(404);
        response.end('no such thing');
        return;
      case '/echo': {
        const pieces = [];
        request.on('data', piece => pieces.push(piece));
        request.on('end', () => {
          response.writeHead(200, { 'x-method': request.method, 'x-seen-one': request.headers['x-one'] ?? '' });
          response.end(Buffer.concat(pieces));
        });
        return;
      }
      case '/sum': {
        // The sum of (position + 1) * byte, worked out here as the page works it out there.
        let sum = 0n;
        let length = 0;
        request.on('data', piece => {
          let part = 0;
          for (let i = 0; i < piece.length; i += 1) part += (length + i + 1) * piece[i];
          sum += BigInt(part);
          length += piece.length;
        });
        request.on('end', () => {
          response.writeHead(200, { 'x-length': String(length) });
          response.end(sum.toString());
        });
        return;
      }
      case '/redirect':
        response.writeHead(302, { location: url.searchParams.get('to') ?? '/hello' });
        response.end();
        return;
      case '/loop':
        response.writeHead(302, { location: '/loop' });
        response.end();
        return;
      case '/slow': {
        const timer = setTimeout(() => response.end('late'), 4000);
        response.on('close', () => clearTimeout(timer));
        return;
      }
      case '/big': {
        const size = Number(url.searchParams.get('size') ?? SIZE);
        response.writeHead(200, { 'content-length': String(size) });
        let at = 0;
        const pump = () => {
          while (at < size) {
            const end = Math.min(at + 65536, size);
            const piece = Buffer.allocUnsafe(end - at);
            for (let i = at; i < end; i += 1) piece[i - at] = bigByte(i);
            at = end;
            if (!response.write(piece)) {
              response.once('drain', pump);
              return;
            }
          }
          response.end();
        };
        pump();
        return;
      }
      default:
        response.writeHead(404);
        response.end();
    }
  });
  return new Promise(resolve => {
    server.listen(0, '127.0.0.1', () => resolve({
      port: server.address().port, seen, cut,
      close: () => new Promise(done => { server.closeAllConnections(); server.close(done); }),
    }));
  });
}

/** A port nothing listens on. */
async function closedPort() {
  const server = http.createServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  await new Promise(resolve => server.close(resolve));
  return port;
}

/** Waits for a file to be gone (the runtime removes the half of a download after the abort). */
async function absent(path, ms) {
  const deadline = Date.now() + ms;
  while (existsSync(path) && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 50));
  return !existsSync(path);
}

/** A TCP echo, a TLS echo and a UDP echo on the loopback, with a count of what each took. */
async function socketServers() {
  const seen = { accepted: 0, ended: 0, handshakes: 0, datagrams: 0 };
  const open = new Set();
  const track = connection => {
    open.add(connection);
    connection.on('close', () => open.delete(connection));
    connection.on('error', () => {});
  };
  const echo = net.createServer(connection => {
    seen.accepted += 1;
    track(connection);
    connection.on('end', () => { seen.ended += 1; });
    connection.pipe(connection);
  });
  const secure = tls.createServer(
    { key: readFileSync(join(TLS_FILES, 'server.key')), cert: readFileSync(join(TLS_FILES, 'server.pem')) },
    connection => {
      seen.handshakes += 1;
      track(connection);
      connection.pipe(connection);
    },
  );
  secure.on('tlsClientError', () => {});
  const udp = dgram.createSocket('udp4');
  udp.on('error', () => {});
  udp.on('message', (message, from) => {
    seen.datagrams += 1;
    udp.send(message, from.port, from.address);
  });
  await Promise.all([
    new Promise(resolve => echo.listen(0, '127.0.0.1', resolve)),
    new Promise(resolve => secure.listen(0, '127.0.0.1', resolve)),
    new Promise(resolve => udp.bind(0, '127.0.0.1', resolve)),
  ]);
  return {
    seen,
    echo: echo.address().port,
    secure: secure.address().port,
    udp: udp.address().port,
    authority: readFileSync(join(TLS_FILES, 'ca.pem'), 'utf8'),
    close: async () => {
      for (const connection of open) connection.destroy();
      await Promise.all([new Promise(resolve => echo.close(resolve)), new Promise(resolve => secure.close(resolve)), new Promise(resolve => udp.close(resolve))]);
    },
  };
}

/** Waits (a few seconds at most) until a condition holds. */
async function eventually(condition, ms) {
  const deadline = Date.now() + ms;
  while (!condition() && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 50));
  return condition();
}

export function netScenarios({ drive }) {
  async function run(name, mode, expectedChecks, env, judge) {
    const base = mkdtempSync(join(os.tmpdir(), 'alef-e2e-http-'));
    const root = join(base, 'root');
    mkdirSync(root);
    const main = await origin();
    const aside = await origin();
    const closed = await closedPort();
    try {
      return await drive({
        name, app: 'modules/net/http',
        replacements: { PORT: String(main.port), CLOSED: String(closed), ROOT: root.replaceAll('\\', '/') },
        targets: { mode, port: main.port, otherPort: aside.port, closed, size: SIZE, root: root.replaceAll('\\', '/') },
        env: { ALEF_HOME: join(base, 'home'), ...(typeof env === 'function' ? env({ main, closed }) : env) },
        expectedChecks,
        judge: () => judge({ root, main, aside, base }),
      });
    } finally {
      await main.close();
      await aside.close();
      rmSync(base, { recursive: true, force: true });
    }
  }

  async function runSocket(name, mode, expectedChecks, env, judge) {
    const base = mkdtempSync(join(os.tmpdir(), 'alef-e2e-socket-'));
    const servers = await socketServers();
    const closed = await closedPort();
    try {
      return await drive({
        name, app: 'modules/net/socket',
        targets: { mode, echo: servers.echo, secure: servers.secure, udp: servers.udp, closed, size: SIZE, authority: servers.authority },
        env: { ALEF_HOME: join(base, 'home'), ...env },
        expectedChecks,
        judge: () => judge(servers.seen),
      });
    } finally {
      await servers.close();
      rmSync(base, { recursive: true, force: true });
    }
  }

  return {
    // The user allowed the addresses: everything is real, and the runner looks at what its servers saw.
    http: () => run('http', 'allowed', HTTP_CHECKS, {}, async ({ root, main, aside }) => {
      const problems = [];
      const file = join(root, 'big.bin');
      if (!existsSync(file)) problems.push('the downloaded file is missing');
      else {
        const bytes = readFileSync(file);
        const expected = createHash('sha256');
        for (let at = 0; at < SIZE; at += 65536) {
          const piece = Buffer.allocUnsafe(Math.min(65536, SIZE - at));
          for (let i = 0; i < piece.length; i += 1) piece[i] = bigByte(at + i);
          expected.update(piece);
        }
        if (bytes.length !== SIZE || createHash('sha256').update(bytes).digest('hex') !== expected.digest('hex')) problems.push('the downloaded file is not what the server sent');
      }
      for (const name of ['missing.bin', 'stopped.bin']) {
        if (!(await absent(join(root, name), 3000))) problems.push(`${name} was left behind`);
      }
      if (existsSync(join(root, '..', 'outside.bin'))) problems.push('a download got out of the scope of fs.write');
      if (aside.seen.length !== 0) problems.push(`the server outside the scope was reached: ${JSON.stringify(aside.seen)}`);
      if (!main.cut.includes('/slow')) problems.push('an aborted request did not close the connection');
      if (!main.cut.includes('/big')) problems.push('a body that the page gave up did not close the connection');
      return problems;
    }),

    socket: () => runSocket('socket', 'allowed', SOCKET_CHECKS, {}, async seen => {
      const problems = [];
      if (!(await eventually(() => seen.ended >= 2, 5000))) problems.push(`the echo server saw ${seen.ended} connection(s) end, expected 2`);
      if (seen.accepted !== 2) problems.push(`the echo server took ${seen.accepted} connection(s), expected 2`);
      if (seen.handshakes !== 1) problems.push(`the TLS server finished ${seen.handshakes} handshake(s), expected 1`);
      if (seen.datagrams !== 1) problems.push(`the UDP server got ${seen.datagrams} datagram(s), expected 1`);
      return problems;
    }),

    // The user chose a stand-in for the sockets: nothing reached the servers.
    'socket-substitute': () => runSocket(
      'socket-substitute', 'substituted', SOCKET_SUBSTITUTE_CHECKS,
      { ALEF_E2E_CONSENT: ['tcp', 'listen', 'udp'].map(kind => `net.socket:${kind}:127.0.0.1:*=substitute`).concat('*=allow').join(';') },
      async seen => {
        const problems = [];
        if (seen.accepted !== 0) problems.push(`the echo server took ${seen.accepted} connection(s)`);
        if (seen.handshakes !== 0) problems.push(`the TLS server finished ${seen.handshakes} handshake(s)`);
        if (seen.datagrams !== 0) problems.push(`the UDP server got ${seen.datagrams} datagram(s)`);
        return problems;
      },
    ),

    // The user chose a stand-in: the network is dead, and nothing reached the servers.
    'http-substitute': () => run(
      'http-substitute', 'substituted', HTTP_SUBSTITUTE_CHECKS,
      ({ main, closed }) => ({
        ALEF_E2E_CONSENT: [...[main.port, closed].map(port => `net.http:http://127.0.0.1:${port}/*=substitute`), '*=allow'].join(';'),
      }),
      async ({ root, main }) => {
        const problems = [];
        if (main.seen.length !== 0) problems.push(`the server was reached: ${JSON.stringify(main.seen)}`);
        if (existsSync(join(root, 'never.bin'))) problems.push('a file was made');
        return problems;
      },
    ),
  };
}
