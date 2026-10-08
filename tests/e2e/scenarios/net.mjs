// SPDX-License-Identifier: MIT OR Apache-2.0
// Scenarios of the network modules (docs/stages/m4-net-cli.md, "Приёмка"): `http`, `socket`, `websocket` and `http.serve`
// against servers of the runner on the loopback, with the right allowed and with a stand-in the user chose.
import { createHash } from 'node:crypto';
import dgram from 'node:dgram';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import https from 'node:https';
import net from 'node:net';
import os from 'node:os';
import { dirname, join } from 'node:path';
import tls from 'node:tls';
import { fileURLToPath } from 'node:url';
import { BIG, wsServer } from '../ws-server.mjs';

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
const WEBSOCKET_CHECKS = [
  'websocket-text-and-bytes-go-both-ways-and-the-server-chooses-a-subprotocol',
  'websocket-a-big-message-comes-in-pieces-and-one-the-page-sends-comes-back',
  'websocket-the-close-is-done-from-either-side-with-its-code-and-reason',
  'websocket-wss-trusts-the-authority-the-page-names-and-no-other',
  'websocket-the-scope-holds-and-a-closed-port-is-the-network',
];
const WEBSOCKET_SUBSTITUTE_CHECKS = ['websocket-a-substituted-network-hangs-until-its-time-is-up'];
const SERVE_CHECKS = [
  'serve-the-page-answers-requests-of-its-own-with-streams-both-ways',
  'serve-a-client-outside-reaches-the-page-and-big-bodies-go-both-ways',
  'serve-the-host-and-the-origin-of-a-request-are-held',
  'serve-a-folder-is-given-without-the-page-and-the-way-out-is-closed',
  'serve-tls-speaks-https-and-only-https',
  'serve-a-websocket-is-taken-from-the-port-of-the-server-of-http',
  'serve-websocket-serve-gives-the-connections-and-holds-the-origin-the-path-and-the-subprotocol',
  'serve-the-scope-of-listen-holds',
];
const SERVE_SUBSTITUTE_CHECKS = ['serve-a-substituted-port-is-given-and-nobody-comes-to-it'];
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

const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

/** Ports nothing listens on, all different. */
async function freePorts(count) {
  const servers = Array.from({ length: count }, () => http.createServer());
  await Promise.all(servers.map(server => new Promise(resolve => server.listen(0, '127.0.0.1', resolve))));
  const ports = servers.map(server => server.address().port);
  await Promise.all(servers.map(server => new Promise(resolve => server.close(resolve))));
  return ports;
}

/** Whether something takes a connection on the port. */
const listening = port => new Promise(resolve => {
  const probe = net.connect(port, '127.0.0.1');
  probe.on('connect', () => { probe.destroy(); resolve(true); });
  probe.on('error', () => resolve(false));
});

/** A request to the server of the page, whole: its status, headers and body. */
function reach(options, payload) {
  return new Promise((resolve, reject) => {
    const request = (options.ca ? https : http).request({ host: '127.0.0.1', agent: false, ...options }, response => {
      const pieces = [];
      response.on('data', piece => pieces.push(piece));
      response.on('end', () => resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(pieces) }));
    });
    request.on('error', reject);
    request.setTimeout(30000, () => request.destroy(new Error('no answer in 30 s')));
    request.end(payload);
  });
}

/** A WebSocket of the runner: the messages come one by one, and a close is a message of its own. */
function dialSocket(url, protocols) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url, protocols);
    socket.binaryType = 'arraybuffer';
    const queue = [];
    const waiting = [];
    const push = item => (waiting.length > 0 ? waiting.shift()(item) : queue.push(item));
    socket.onmessage = event => push({ data: event.data });
    socket.onclose = event => {
      push({ closed: { code: event.code, reason: event.reason } });
      reject(new Error('the WebSocket did not open'));
    };
    socket.onerror = () => {};
    socket.onopen = () => resolve({
      protocol: socket.protocol,
      send: data => socket.send(data),
      close: (code, reason) => socket.close(code, reason),
      next: (ms = 20000) => new Promise((done, fail) => {
        if (queue.length > 0) {
          done(queue.shift());
          return;
        }
        const timer = setTimeout(() => fail(new Error('no message in time')), ms);
        waiting.push(item => {
          clearTimeout(timer);
          done(item);
        });
      }),
    });
  });
}

/** A request that asks for a WebSocket, with the Origin it is given: the status of the answer. */
const asking = (port, path, claimed) => reach({
  port,
  path,
  headers: {
    connection: 'Upgrade', upgrade: 'websocket', 'sec-websocket-version': '13', 'sec-websocket-key': 'dGhlIHNhbXBsZSBub25jZQ==', ...(claimed === undefined ? {} : { origin: claimed }),
  },
}).then(answer => answer.status);

/**
 * The client of the runner for the server of the page: it comes once the port takes connections, sends
 * what the page must see (big bodies both ways, the folder, TLS) and what it must not (another Host or
 * Origin, a way out of the folder), and says it is finished. `problems()` waits for that.
 */
function serveClient({ port, secure, wsport, authority, size }) {
  const problems = [];
  const body = Buffer.allocUnsafe(3 * 1024 * 1024);
  for (let at = 0; at < body.length; at += 1) body[at] = bigByte(at);
  const expect = (what, condition) => { if (!condition) problems.push(what); };
  const at = path => ({ port, path });
  const attempt = async (what, steps) => {
    try {
      await steps();
    } catch (error) {
      problems.push(`${what}: ${error?.message ?? error}`);
    }
  };
  const finished = (async () => {
    const deadline = Date.now() + 60000;
    while (!(await listening(port))) {
      if (Date.now() > deadline) {
        problems.push('the server of the page did not take connections in a minute');
        return;
      }
      await pause(50);
    }
    await attempt('the first request', async () => {
      const answer = await reach({ ...at('/from-runner?x=1'), headers: { origin: `http://127.0.0.1:${port}` } });
      expect('a request of the client is answered by the page', answer.status === 200 && answer.body.toString() === 'page: GET /from-runner?x=1');
    });
    await attempt('a big body both ways', async () => {
      const echoed = await reach({ ...at('/echo'), method: 'POST' }, body);
      expect('the body came back whole through the page', echoed.status === 200 && echoed.headers['x-method'] === 'POST' && Buffer.compare(echoed.body, body) === 0);
      const big = await reach(at(`/big?size=${size}`));
      let wrong = big.body.length !== size;
      for (let place = 0; !wrong && place < size; place += 1) wrong = big.body[place] !== bigByte(place);
      expect('a big answer of the page came whole', !wrong);
    });
    await attempt('a Host that is not the server\'s', async () => {
      const answer = await reach({ ...at('/evil-host'), headers: { host: 'evil.test' } });
      expect(`another Host got ${answer.status}, expected 421`, answer.status === 421);
    });
    await attempt('an Origin that is not the server\'s', async () => {
      for (const claimed of ['http://evil.test', 'null', `http://127.0.0.1:${port + 1}`]) {
        const answer = await reach({ ...at('/evil-origin'), headers: { origin: claimed } });
        expect(`the Origin ${claimed} got ${answer.status}, expected 403`, answer.status === 403);
      }
    });
    await attempt('the folder', async () => {
      const file = await reach(at('/hello.txt'));
      expect('a file of the folder was given', file.status === 200 && file.body.toString() === 'static hello');
      expect('the media type of the file', file.headers['content-type'] === 'text/plain; charset=utf-8' && file.headers['x-content-type-options'] === 'nosniff');
      const home = await reach(at('/'));
      expect('the index of the folder', home.status === 200 && home.body.toString() === '<h1>home</h1>');
      const head = await reach({ ...at('/hello.txt'), method: 'HEAD' });
      expect('HEAD gives the length and no body', head.status === 200 && head.headers['content-length'] === '12' && head.body.length === 0);
      for (const path of ['/..%2fsecret.txt', '/%2e%2e/secret.txt']) {
        const out = await reach(at(path));
        expect(`${path} did not leave the folder`, out.status === 404 && out.body.toString() === 'the page does not know that');
      }
    });
    await attempt('a WebSocket on the port of the server of HTTP', async () => {
      const socket = await dialSocket(`ws://127.0.0.1:${port}/socket`, ['chat', 'superchat']);
      expect('the page chose superchat', socket.protocol === 'superchat');
      socket.send('héllo');
      expect('a text message came back', (await socket.next()).data === 'echo:héllo');
      const big = Buffer.allocUnsafe(4 * 1024 * 1024);
      let sum = 0;
      for (let place = 0; place < big.length; place += 1) {
        big[place] = bigByte(place);
        sum += big[place];
      }
      socket.send(big);
      expect('the page took 4 MiB whole', (await socket.next()).data === `got ${big.length} ${sum}`);
      socket.close(4001, 'bye');
      const closed = (await socket.next()).closed;
      expect('the close came back with its code', closed?.code === 4001 && closed?.reason === 'bye');
      expect('an Origin that is not the server\'s is refused on the upgrade too', (await asking(port, '/socket', 'http://evil.test')) === 403);
    });
    await attempt('a WebSocket on the server of WebSocket', async () => {
      const socket = await dialSocket(`ws://127.0.0.1:${wsport}/ws`, ['chat']);
      expect('the page chose chat', socket.protocol === 'chat');
      socket.send('one');
      expect('a text message came back', (await socket.next()).data === 'echo:one');
      socket.send('bye');
      const closed = (await socket.next()).closed;
      expect('the page closed with its code and reason', closed?.code === 4000 && closed?.reason === 'done');
      expect('another Origin is refused', (await asking(wsport, '/ws', 'http://evil.test')) === 403);
      expect('another path is a 404', (await asking(wsport, '/other')) === 404);
      expect('what is no offer is a 426', (await reach({ port: wsport, path: '/ws' })).status === 426);
    });
    await attempt('TLS', async () => {
      const answer = await reach({ port: secure, path: '/secure', ca: authority });
      expect('the page answers over TLS', answer.status === 200 && answer.body.toString() === 'page: GET /secure');
      const plain = await reach({ port: secure, path: '/plain' }).then(() => 'answered', () => 'refused');
      expect('a server with TLS does not answer in plain', plain === 'refused');
    });
  })().finally(() => reach(at('/finish')).catch(() => {}));
  return { problems: async () => { await finished; return problems; }, stop: () => {} };
}

/** The runner looks for a listener on the ports while the page runs: for a port the user substituted there must be none. */
function watchPorts(ports) {
  const heard = new Set();
  const state = { watching: true };
  const loop = (async () => {
    while (state.watching) {
      for (const port of ports) if (await listening(port)) heard.add(port);
      await pause(50);
    }
  })();
  const stop = async () => { state.watching = false; await loop; };
  return { problems: async () => { await stop(); return [...heard].map(port => `something listens on the port ${port} the user substituted`); }, stop };
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

  async function runWebsocket(name, mode, expectedChecks, env, judge) {
    const base = mkdtempSync(join(os.tmpdir(), 'alef-e2e-websocket-'));
    const plain = await wsServer();
    const secure = await wsServer({ tls: { key: readFileSync(join(TLS_FILES, 'server.key')), cert: readFileSync(join(TLS_FILES, 'server.pem')) } });
    const closed = await closedPort();
    try {
      return await drive({
        name, app: 'modules/net/websocket',
        replacements: { PORT: String(plain.port), SECURE: String(secure.port), CLOSED: String(closed) },
        targets: { mode, port: plain.port, secure: secure.port, closed, size: BIG, authority: readFileSync(join(TLS_FILES, 'ca.pem'), 'utf8') },
        env: { ALEF_HOME: join(base, 'home'), ...(typeof env === 'function' ? env({ plain, secure }) : env) },
        expectedChecks,
        judge: () => judge({ plain, secure }),
      });
    } finally {
      await plain.close();
      await secure.close();
      rmSync(base, { recursive: true, force: true });
    }
  }

  /** The page is the server; `watch` is the client of the runner (or the watcher of the ports) beside it. */
  async function runServe(name, mode, expectedChecks, env, watch) {
    const base = mkdtempSync(join(os.tmpdir(), 'alef-e2e-serve-'));
    const root = join(base, 'root');
    mkdirSync(join(root, 'public'), { recursive: true });
    writeFileSync(join(root, 'secret.txt'), 'the secret');
    writeFileSync(join(root, 'public', 'hello.txt'), 'static hello');
    writeFileSync(join(root, 'public', 'index.html'), '<h1>home</h1>');
    const [port, secure, closed, wsport] = await freePorts(4);
    const authority = readFileSync(join(TLS_FILES, 'ca.pem'), 'utf8');
    const client = watch({ port, secure, wsport, authority, size: SIZE });
    try {
      return await drive({
        name, app: 'modules/net/serve',
        replacements: { PORT: String(port), SECURE: String(secure), WSPORT: String(wsport), ROOT: root.replaceAll('\\', '/') },
        targets: {
          mode, port, secure, wsport, closed, size: SIZE, root: root.replaceAll('\\', '/'),
          cert: readFileSync(join(TLS_FILES, 'server.pem'), 'utf8'), key: readFileSync(join(TLS_FILES, 'server.key'), 'utf8'),
        },
        env: { ALEF_HOME: join(base, 'home'), ...(typeof env === 'function' ? env({ port, secure, wsport }) : env) },
        expectedChecks,
        judge: () => client.problems(),
      });
    } finally {
      await client.stop();
      rmSync(base, { recursive: true, force: true });
    }
  }

  return {
    // The page is a server: a client of the runner comes to it, the runner looks at what it got.
    serve: () => runServe('serve', 'allowed', SERVE_CHECKS, {}, serveClient),

    // The user chose a stand-in for the port: the page gets an address, and nobody listens there.
    'serve-substitute': () => runServe(
      'serve-substitute', 'substituted', SERVE_SUBSTITUTE_CHECKS,
      ({ port, wsport }) => ({ ALEF_E2E_CONSENT: `net.socket:listen:127.0.0.1:${port}=substitute;net.socket:listen:127.0.0.1:${wsport}=substitute;*=allow` }),
      ({ port, secure, wsport }) => watchPorts([port, secure, wsport]),
    ),

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

    websocket: () => runWebsocket('websocket', 'allowed', WEBSOCKET_CHECKS, {}, async ({ plain }) => {
      const problems = [];
      const first = plain.seen.headers[0];
      if (first?.origin !== 'https://app.test' || first?.protocol !== 'superchat') problems.push(`the server saw ${JSON.stringify(first)}`);
      if (!(await eventually(() => plain.seen.closes.some(close => close.code === 4000 && close.reason === 'done'), 5000))) problems.push(`the server did not see the close with 4000: ${JSON.stringify(plain.seen.closes)}`);
      if (plain.seen.messages < 4) problems.push(`the server got ${plain.seen.messages} message(s), expected at least 4`);
      return problems;
    }),

    // The user chose a stand-in for the network: nothing reached the servers.
    'websocket-substitute': () => runWebsocket(
      'websocket-substitute', 'substituted', WEBSOCKET_SUBSTITUTE_CHECKS,
      ({ plain, secure }) => ({
        ALEF_E2E_CONSENT: [plain.port, secure.port].map((port, index) => `net.http:${index === 0 ? 'ws' : 'wss'}://127.0.0.1:${port}/*=substitute`).concat('*=allow').join(';'),
      }),
      async ({ plain, secure }) => {
        const problems = [];
        if (plain.seen.upgrades !== 0) problems.push(`the server was reached: ${plain.seen.upgrades}`);
        if (secure.seen.upgrades !== 0) problems.push(`the secure server was reached: ${secure.seen.upgrades}`);
        return problems;
      },
    ),

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
