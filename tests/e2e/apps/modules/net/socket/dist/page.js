// The module socket through the real transport: TCP against an echo server of the runner and against a
// listener of the page itself, TLS with the authority of the test, UDP between two sockets and against a
// server of the runner, and the scope that holds for every command. In `substituted` mode the user chose a
// stand-in: the network is dead, and the runner looks at its servers to see that nothing arrived.
import { MIB, api, guard, pattern, rejection, report, same, sleep, suite, verdict } from './harness.js';

const { socket } = api;
const text = bytes => new TextDecoder().decode(bytes);
const bytes = string => new TextEncoder().encode(string);

const expectCode = async (what, promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`${what}: ${error?.code ?? error?.name ?? 'it succeeded'}, expected ${code}`);
  return error;
};

/** Reads exactly `count` bytes from a reader of a socket. */
async function readExactly(reader, count) {
  const out = new Uint8Array(count);
  let at = 0;
  while (at < count) {
    const { done, value } = await reader.read();
    if (done) throw new Error(`the stream ended after ${at} of ${count} bytes`);
    out.set(value.subarray(0, count - at), at);
    at += value.length;
  }
  return out;
}

/** Waits for the next datagram, or says that none came. */
const next = (iterator, ms) => Promise.race([iterator.next().then(result => result.value), sleep(ms).then(() => null)]);

/** An iteration that waits for what never comes ends when its socket is closed (it cannot be left any other way). */
async function endsSoon(iterator, what) {
  const outcome = await Promise.race([iterator.next(), sleep(2000).then(() => 'hung')]);
  if (outcome === 'hung' || !outcome.done) throw new Error(`${what} did not end`);
}

async function allowed(t, check) {
  await check('socket-tcp-carries-a-big-body-both-ways-through-an-echo-server', async () => {
    const started = performance.now();
    const conn = await socket.connect({ host: '127.0.0.1', port: t.echo });
    if (conn.remoteAddress.port !== t.echo || conn.localAddress.host !== '127.0.0.1') throw new Error(JSON.stringify([conn.localAddress, conn.remoteAddress]));
    const data = pattern(t.size);
    const writer = conn.writable.getWriter();
    const sending = (async () => {
      for (let at = 0; at < data.length; at += 256 * 1024) await writer.write(data.slice(at, at + 256 * 1024));
      await writer.close();
    })();
    const reader = conn.readable.getReader();
    let got = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      for (let i = 0; i < value.length; i += 1) {
        if (value[i] !== data[got + i]) throw new Error(`byte ${got + i} changed`);
      }
      got += value.length;
    }
    await sending;
    if (got !== t.size) throw new Error(`${got} of ${t.size} bytes came back`);
    await conn.close();
    return `${t.size / MIB} MiB each way in ${Math.round(performance.now() - started)} ms`;
  });

  await check('socket-a-listener-takes-a-connection-of-the-page-itself', async () => {
    const server = await socket.listen({ port: 0 });
    const { port } = server.localAddress;
    if (server.localAddress.host !== '127.0.0.1' || port === 0) throw new Error(JSON.stringify(server.localAddress));
    const taken = (async () => {
      for await (const conn of server) return conn;
      return null;
    })();
    const client = await socket.connect({ host: '127.0.0.1', port });
    const peer = await taken;
    if (peer === null) throw new Error('the server took nobody');
    if (peer.remoteAddress.port !== client.localAddress.port) throw new Error(`${peer.remoteAddress.port} is not ${client.localAddress.port}`);
    const toPeer = client.writable.getWriter();
    const toClient = peer.writable.getWriter();
    const fromClient = client.readable.getReader();
    const fromPeer = peer.readable.getReader();
    await toPeer.write(bytes('ping'));
    if (text(await readExactly(fromPeer, 4)) !== 'ping') throw new Error('ping');
    await toClient.write(bytes('pong'));
    if (text(await readExactly(fromClient, 4)) !== 'pong') throw new Error('pong');
    await toPeer.close();
    const { done } = await fromPeer.read();
    if (!done) throw new Error('the end of the client was not the end of the stream of the peer');
    await server.close();
    const late = await rejection(socket.connect({ host: '127.0.0.1', port, timeout: 3000 }));
    if (late?.code !== 'NETWORK') throw new Error(`a closed server took a connection: ${late?.code ?? 'it connected'}`);
    await client.close();
    await peer.close();
  });

  await check('socket-tls-trusts-the-authority-the-page-names-and-no-other', async () => {
    const conn = await socket.connect({ host: '127.0.0.1', port: t.secure, tls: { ca: t.authority } });
    const writer = conn.writable.getWriter();
    await writer.write(bytes('a secret'));
    if (text(await readExactly(conn.readable.getReader(), 8)) !== 'a secret') throw new Error('the secret did not come back');
    await conn.close();
    const unknown = await expectCode('the roots of Mozilla', socket.connect({ host: '127.0.0.1', port: t.secure, tls: true }), 'NETWORK');
    if (!unknown.message.startsWith('the TLS handshake failed')) throw new Error(unknown.message);
    await expectCode('another name', socket.connect({ host: '127.0.0.1', port: t.secure, tls: { ca: t.authority, serverName: 'other.test' } }), 'NETWORK');
    await expectCode('not a certificate', socket.connect({ host: '127.0.0.1', port: t.secure, tls: { ca: 'nothing' } }), 'INVALID_ARGUMENT');
  });

  await check('socket-udp-goes-between-two-sockets-and-to-a-server', async () => {
    const first = await socket.udp();
    const second = await socket.udp();
    const heard = second[Symbol.asyncIterator]();
    await first.send('hello', '127.0.0.1', second.localAddress.port);
    const datagram = await next(heard, 5000);
    if (datagram === null || text(datagram.data) !== 'hello' || datagram.port !== first.localAddress.port) throw new Error(JSON.stringify(datagram));
    const echoed = first[Symbol.asyncIterator]();
    const big = pattern(8000);
    await first.send(big, '127.0.0.1', t.udp);
    const back = await next(echoed, 5000);
    if (back === null || back.port !== t.udp || !same(back.data, big)) throw new Error('the datagram did not come back from the server');
    await heard.return();
    await echoed.return();
    await first.close();
    await second.close();
  });

  await check('socket-the-scope-holds-for-every-command-and-a-closed-port-is-the-network', async () => {
    const denied = await expectCode('another name for the same place', socket.connect({ host: 'localhost', port: t.echo }), 'PERMISSION_DENIED');
    if (denied.details?.permission !== 'net.socket') throw new Error(JSON.stringify(denied.details));
    await expectCode('another host', socket.connect({ host: 'example.com', port: 80 }), 'PERMISSION_DENIED');
    await expectCode('a port for every address', socket.listen({ host: '0.0.0.0', port: 0 }), 'PERMISSION_DENIED');
    await expectCode('a udp socket for every address', socket.udp({ host: '0.0.0.0' }), 'PERMISSION_DENIED');
    const udp = await socket.udp();
    await expectCode('a datagram elsewhere', udp.send('x', '10.0.0.1', 9), 'PERMISSION_DENIED');
    await udp.close();
    await expectCode('nobody listens', socket.connect({ host: '127.0.0.1', port: t.closed }), 'NETWORK');
  });

  await check('socket-a-closed-socket-ends-its-streams-and-the-server-sees-the-end', async () => {
    const conn = await socket.connect({ host: '127.0.0.1', port: t.echo });
    const writer = conn.writable.getWriter();
    await writer.write(bytes('last'));
    await conn.close();
    await conn.close();
    const settled = await Promise.race([
      conn.readable.getReader().read().then(() => 'settled', () => 'settled'),
      sleep(3000).then(() => 'hung'),
    ]);
    if (settled !== 'settled') throw new Error('the readable of a closed socket did not end');
  });
}

async function substituted(t, check) {
  await check('socket-a-substituted-network-is-dead-and-takes-no-port', async () => {
    const started = performance.now();
    await expectCode('a connection', socket.connect({ host: '127.0.0.1', port: t.echo, timeout: 400 }), 'TIMEOUT');
    if (performance.now() - started < 350) throw new Error('it did not hang');
    await expectCode('a connection with TLS', socket.connect({ host: '127.0.0.1', port: t.secure, tls: { ca: t.authority }, timeout: 300 }), 'TIMEOUT');

    const server = await socket.listen({ port: 0 });
    const taken = server[Symbol.asyncIterator]();
    if (await next(taken, 300) !== null) throw new Error('a stand-in took a connection');
    await server.close();
    await endsSoon(taken, 'the iteration of a closed server');

    const udp = await socket.udp();
    const heard = udp[Symbol.asyncIterator]();
    await udp.send('lost', '127.0.0.1', t.udp);
    if (await next(heard, 400) !== null) throw new Error('a stand-in got a datagram');
    await udp.close();
    await endsSoon(heard, 'the iteration of a closed socket');
  });
}

async function main() {
  const t = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  if (t.mode === 'substituted') await substituted(t, check);
  else await allowed(t, check);
  await report(`mode ${t.mode}`);
  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
