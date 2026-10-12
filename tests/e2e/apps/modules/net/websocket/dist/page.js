// The module websocket through the real transport against servers of the runner on the loopback: messages
// of text and bytes each way, a big message in pieces, the close from either side, a secure server with the
// authority of the test, the scope that holds. In `substituted` mode the user chose a stand-in: the network
// is dead, and the runner looks at its server to see that nothing arrived.
import { api, guard, pattern, rejection, report, same, sleep, suite, verdict } from './harness.js';

const { websocket } = api;

const expectCode = async (what, promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`${what}: ${error?.code ?? error?.name ?? 'it succeeded'}, expected ${code}`);
  return error;
};

/** The next message of an iterator, or `null` when none comes in time. */
const next = async (iterator, ms = 5000) => {
  const outcome = await Promise.race([iterator.next(), sleep(ms).then(() => 'late')]);
  if (outcome === 'late') throw new Error('no message came');
  return outcome.done ? null : outcome.value;
};

async function allowed(t, check) {
  const ws = path => `ws://127.0.0.1:${t.port}${path}`;

  await check('websocket-text-and-bytes-go-both-ways-and-the-server-chooses-a-subprotocol', async () => {
    const connection = await websocket.connect(ws('/echo'), { protocols: ['chat', 'superchat'], headers: { Origin: 'https://app.test' } });
    if (connection.protocol !== 'superchat') throw new Error(`the subprotocol is ${JSON.stringify(connection.protocol)}`);
    const messages = connection[Symbol.asyncIterator]();
    const text = 'héllo — мир 🌍';
    await connection.send(text);
    const first = await next(messages);
    if (first?.type !== 'text' || first.data !== text) throw new Error(JSON.stringify(first));
    const every = Uint8Array.from({ length: 256 }, (_, index) => index);
    await connection.send(every);
    const second = await next(messages);
    if (second?.type !== 'binary' || !same(second.data, every)) throw new Error('the bytes changed');
    await connection.send('');
    const empty = await next(messages);
    if (empty?.type !== 'text' || empty.data !== '') throw new Error(JSON.stringify(empty));
    await messages.return();
    const info = await connection.closed;
    if (info.code !== 1006 && info.code !== 1000) throw new Error(JSON.stringify(info));
  });

  await check('websocket-a-big-message-comes-in-pieces-and-one-the-page-sends-comes-back', async () => {
    const connection = await websocket.connect(ws('/big'));
    const messages = connection[Symbol.asyncIterator]();
    const big = await next(messages, 20000);
    const expected = pattern(t.size);
    if (big?.type !== 'binary' || !same(big.data, expected)) throw new Error('the big message did not come whole');
    const data = pattern(180 * 1024);
    await connection.send(data);
    const back = await next(messages, 20000);
    if (back?.type !== 'binary' || !same(back.data, data)) throw new Error('the message did not come back');
    await messages.return();
  });

  await check('websocket-the-close-is-done-from-either-side-with-its-code-and-reason', async () => {
    const mine = await websocket.connect(ws('/echo'));
    const draining = (async () => { for await (const _ of mine) { /* nothing is sent */ } })();
    await mine.close(4000, 'done');
    await draining;
    const info = await mine.closed;
    if (info.code !== 4000 || info.reason !== 'done' || !info.clean) throw new Error(`the answer: ${JSON.stringify(info)}`);

    const theirs = await websocket.connect(ws('/bye'));
    const got = [];
    for await (const message of theirs) got.push(`${message.type}:${message.data}`);
    if (got.join() !== 'text:last') throw new Error(got.join());
    const said = await theirs.closed;
    if (said.code !== 4001 || said.reason !== 'bye' || !said.clean) throw new Error(JSON.stringify(said));
  });

  await check('websocket-wss-trusts-the-authority-the-page-names-and-no-other', async () => {
    const secure = `wss://127.0.0.1:${t.secure}/echo`;
    const connection = await websocket.connect(secure, { ca: t.authority });
    const messages = connection[Symbol.asyncIterator]();
    await connection.send('secret');
    const echoed = await next(messages);
    if (echoed?.data !== 'secret') throw new Error(JSON.stringify(echoed));
    await messages.return();
    const unknown = await expectCode('the roots of Mozilla', websocket.connect(secure), 'NETWORK');
    if (!unknown.message.startsWith('the TLS handshake failed')) throw new Error(unknown.message);
    await expectCode('not a certificate', websocket.connect(secure, { ca: 'nothing' }), 'INVALID_ARGUMENT');
  });

  await check('websocket-the-scope-holds-and-a-closed-port-is-the-network', async () => {
    const denied = await expectCode('another port', websocket.connect(`ws://127.0.0.1:${t.port + 1}/`), 'PERMISSION_DENIED');
    if (denied.details?.permission !== 'net.http') throw new Error(JSON.stringify(denied.details));
    await expectCode('another scheme', websocket.connect(`wss://127.0.0.1:${t.port}/`), 'PERMISSION_DENIED');
    await expectCode('another host', websocket.connect('ws://example.com/'), 'PERMISSION_DENIED');
    await expectCode('nobody listens', websocket.connect(`ws://127.0.0.1:${t.closed}/`), 'NETWORK');
    await expectCode('a header of the handshake', websocket.connect(ws('/echo'), { headers: { 'Sec-WebSocket-Key': 'x' } }), 'INVALID_ARGUMENT');
  });
}

async function substituted(t, check) {
  await check('websocket-a-substituted-network-hangs-until-its-time-is-up', async () => {
    const started = performance.now();
    await expectCode('a connection', websocket.connect(`ws://127.0.0.1:${t.port}/echo`, { timeout: 400 }), 'TIMEOUT');
    if (performance.now() - started < 350) throw new Error('it did not hang');
    await expectCode('a connection with TLS', websocket.connect(`wss://127.0.0.1:${t.secure}/echo`, { ca: t.authority, timeout: 300 }), 'TIMEOUT');
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
