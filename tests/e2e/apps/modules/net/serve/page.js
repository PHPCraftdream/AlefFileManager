// The module http.serve through the real transport: the page is a server on the loopback, and a client of
// the runner comes to it from outside (big bodies both ways, a Host and an Origin that are not the server's,
// the files of a folder, TLS, WebSockets taken from the port of the server of HTTP and from a server of their
// own). In `substituted` mode the user chose a stand-in for the port: the page gets
// an address, and the runner sees that nobody listens there.
import { api, guard, pattern, rejection, report, same, sleep, suite, verdict } from './harness.js';

const { http, websocket } = api;

const expectCode = async (what, promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`${what}: ${error?.code ?? error?.name ?? 'it succeeded'}, expected ${code}`);
  return error;
};

/** A body of `size` bytes that comes out piece by piece, as a stream. */
function streamOf(size) {
  const whole = pattern(size);
  let at = 0;
  return new ReadableStream({
    pull(controller) {
      if (at >= size) controller.close();
      else {
        controller.enqueue(whole.slice(at, Math.min(at + 65536, size)));
        at += 65536;
      }
    },
  });
}

/** The sum of the bytes of a message: what the page tells of a big message it took. */
const checksum = bytes => bytes.reduce((sum, byte) => sum + byte, 0);

/** The next message of an iterator, or an error when none comes in time. */
const next = async (iterator, ms = 10000) => {
  const outcome = await Promise.race([iterator.next(), sleep(ms).then(() => 'late')]);
  if (outcome === 'late') throw new Error('no message came');
  return outcome.done ? null : outcome.value;
};

/** Answers a connection: text comes back as `echo:text`, a binary message as its length and sum, `bye` closes with 4000. */
async function echoLoop(connection, heard) {
  for await (const message of connection) {
    if (message.type === 'binary') {
      heard.push(`binary ${message.data.length}`);
      await connection.send(`got ${message.data.length} ${checksum(message.data)}`);
    } else if (message.data === 'bye') {
      heard.push('bye');
      await connection.close(4000, 'done');
      return;
    } else {
      heard.push(message.data);
      await connection.send(`echo:${message.data}`);
    }
  }
}

const logged = what => error => console.error(`ALEF_E2E-page ${what} failed ${error?.message ?? error}`);

/** Answers every request of a server the way the runner's client expects; `asked` is what the page was asked. */
async function answer(server, asked, finish, viaHttp) {
  for await (const request of server) {
    asked.push(request.url);
    const path = new URL(request.url, 'http://page').pathname;
    const reply = async () => {
      switch (path) {
        case '/hello':
          await request.respond({ body: 'hello from the page', headers: { 'x-seen': request.headers.get('x-one') ?? '' } });
          return;
        case '/echo':
          await request.respond({ body: request.body ?? '', headers: { 'x-method': request.method } });
          return;
        case '/big':
          await request.respond({ body: streamOf(Number(new URL(request.url, 'http://page').searchParams.get('size'))), headers: { 'content-type': 'application/octet-stream' } });
          return;
        case '/finish':
          await request.respond({ body: 'bye' });
          finish();
          return;
        case '/socket':
          if (!request.upgradable) {
            await request.respond({ status: 426, body: 'a WebSocket, please' });
            return;
          }
          echoLoop(await request.upgrade({ protocol: request.protocols.includes('superchat') ? 'superchat' : undefined }), viaHttp).catch(logged('a WebSocket of the server of HTTP'));
          return;
        case '/from-runner':
        case '/secure':
          await request.respond({ body: `page: ${request.method} ${request.url}` });
          return;
        default:
          await request.respond({ status: 404, body: 'the page does not know that' });
      }
    };
    reply().catch(error => console.error(`ALEF_E2E-page answer failed ${error?.message ?? error}`));
  }
}

async function allowed(t, check) {
  const asked = [];
  const askedSecure = [];
  let finish;
  const finished = new Promise(resolve => { finish = resolve; });
  const viaHttp = [];
  const viaServe = [];
  const server = await http.serve({ port: t.port, files: `${t.root}/public`, answerTimeout: 30000 });
  const secure = await http.serve({ port: t.secure, tls: { cert: t.cert, key: t.key } });
  const sockets = await websocket.serve({ port: t.wsport, path: '/ws', protocols: ['chat', 'superchat'], origins: ['https://app.test'] });
  answer(server, asked, finish, viaHttp).catch(logged('the server'));
  answer(secure, askedSecure, () => {}, []).catch(logged('the secure server'));
  (async () => {
    for await (const connection of sockets) echoLoop(connection, viaServe).catch(logged('a WebSocket of the server of WebSocket'));
  })().catch(logged('the server of WebSocket'));
  const waitForClient = () => Promise.race([finished, sleep(90000).then(() => { throw new Error('the client of the runner did not finish'); })]);

  await check('serve-the-page-answers-requests-of-its-own-with-streams-both-ways', async () => {
    if (server.address.port !== t.port || server.secure || server.url !== `http://127.0.0.1:${t.port}`) throw new Error(JSON.stringify([server.address, server.secure, server.url]));
    const hello = await http.request(`${server.url}/hello`, { headers: { 'X-One': '1' } });
    if (hello.status !== 200 || hello.headers.get('x-seen') !== '1' || await hello.text() !== 'hello from the page') throw new Error('the answer to /hello');
    const body = pattern(3 * 1024 * 1024);
    const echoed = await http.request(`${server.url}/echo`, { method: 'POST', body });
    if (echoed.headers.get('x-method') !== 'POST' || !same(await echoed.bytes(), body)) throw new Error('the body did not come back whole');
    const big = await http.request(`${server.url}/big?size=${t.size}`);
    if (!same(await big.bytes(), pattern(t.size))) throw new Error('the big answer is not what the page sent');
    const unknown = await http.request(`${server.url}/nothing`);
    if (unknown.status !== 404) throw new Error(`${unknown.status}`);
  });

  await check('serve-a-client-outside-reaches-the-page-and-big-bodies-go-both-ways', async () => {
    await waitForClient();
    if (!asked.includes('/from-runner?x=1')) throw new Error(`the page was not asked by the client: ${JSON.stringify(asked)}`);
    if (!asked.includes('/echo') || !asked.some(url => url.startsWith('/big'))) throw new Error('the big bodies did not reach the page');
  });

  await check('serve-the-host-and-the-origin-of-a-request-are-held', async () => {
    const strange = asked.filter(url => url.startsWith('/evil'));
    if (strange.length > 0) throw new Error(`requests of another host or origin reached the page: ${JSON.stringify(strange)}`);
  });

  await check('serve-a-folder-is-given-without-the-page-and-the-way-out-is-closed', async () => {
    if (asked.some(url => url === '/hello.txt' || url === '/')) throw new Error(`a file of the folder came to the page: ${JSON.stringify(asked)}`);
    if (!asked.includes('/..%2fsecret.txt') || !asked.includes('/%2e%2e/secret.txt')) throw new Error(`the way out of the folder was not left to the page: ${JSON.stringify(asked)}`);
  });

  await check('serve-tls-speaks-https-and-only-https', async () => {
    if (!secure.secure || !secure.url.startsWith('https://')) throw new Error(`${secure.secure} ${secure.url}`);
    if (!askedSecure.includes('/secure')) throw new Error(`the page was not asked over TLS: ${JSON.stringify(askedSecure)}`);
    if (askedSecure.length !== 1) throw new Error(`more than the one request came over TLS: ${JSON.stringify(askedSecure)}`);
  });

  await check('serve-a-websocket-is-taken-from-the-port-of-the-server-of-http', async () => {
    const mine = await websocket.connect(`ws://127.0.0.1:${t.port}/socket`, { protocols: ['chat', 'superchat'] });
    if (mine.protocol !== 'superchat') throw new Error(`the subprotocol is ${JSON.stringify(mine.protocol)}`);
    const messages = mine[Symbol.asyncIterator]();
    await mine.send('self');
    const echoed = await next(messages);
    if (echoed?.data !== 'echo:self') throw new Error(JSON.stringify(echoed));
    await mine.close();
    await waitForClient();
    for (const wanted of ['héllo', 'binary 4194304']) {
      if (!viaHttp.includes(wanted)) throw new Error(`the client of the runner was not heard: ${JSON.stringify(viaHttp)}`);
    }
  });

  await check('serve-websocket-serve-gives-the-connections-and-holds-the-origin-the-path-and-the-subprotocol', async () => {
    if (sockets.address.port !== t.wsport || sockets.url !== `ws://127.0.0.1:${t.wsport}/ws`) throw new Error(JSON.stringify([sockets.address, sockets.url]));
    const address = `ws://127.0.0.1:${t.wsport}/ws`;
    const mine = await websocket.connect(address, { protocols: ['superchat'], headers: { Origin: 'https://app.test' } });
    if (mine.protocol !== 'superchat') throw new Error(`the subprotocol is ${JSON.stringify(mine.protocol)}`);
    const messages = mine[Symbol.asyncIterator]();
    await mine.send('self');
    const echoed = await next(messages);
    if (echoed?.data !== 'echo:self') throw new Error(JSON.stringify(echoed));
    await mine.close();
    await expectCode('another origin', websocket.connect(address, { headers: { Origin: 'http://evil.test' } }), 'NETWORK');
    await expectCode('another path', websocket.connect(`ws://127.0.0.1:${t.wsport}/other`), 'NETWORK');
    await expectCode('no subprotocol in common', websocket.connect(address, { protocols: ['other'] }), 'NETWORK');
    await waitForClient();
    for (const wanted of ['one', 'bye']) {
      if (!viaServe.includes(wanted)) throw new Error(`the client of the runner was not heard: ${JSON.stringify(viaServe)}`);
    }
  });

  await check('serve-the-scope-of-listen-holds', async () => {
    await expectCode('a port that is not listed', http.serve({ port: t.closed }), 'PERMISSION_DENIED');
    await expectCode('an address that is not the loopback', http.serve({ host: '0.0.0.0', port: t.port }), 'PERMISSION_DENIED');
    await expectCode('a folder outside the scope of fs.read', http.serve({ port: t.port, files: `${t.root}/..` }), 'PERMISSION_DENIED');
    await server.close();
    await secure.close();
    await sockets.close();
    await server.close();
  });
}

async function substituted(t, check) {
  await check('serve-a-substituted-port-is-given-and-nobody-comes-to-it', async () => {
    const server = await http.serve({ port: t.port });
    if (server.address.port !== t.port || server.address.host !== '127.0.0.1') throw new Error(JSON.stringify(server.address));
    const sockets = await websocket.serve({ port: t.wsport });
    if (sockets.address.port !== t.wsport) throw new Error(JSON.stringify(sockets.address));
    const outcome = await Promise.race([
      server[Symbol.asyncIterator]().next().then(() => 'a request came'),
      sleep(1500).then(() => 'quiet'),
    ]);
    if (outcome !== 'quiet') throw new Error(outcome);
    await server.close();
    await sockets.close();
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
