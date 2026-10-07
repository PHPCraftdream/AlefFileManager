// The module http through the real transport against servers of the runner on the loopback: requests and
// answers, a big body up and down as streams, redirects held against the scope, time and network errors,
// an abort that reaches the connection, a download into a file. In `substituted` mode the user chose a
// stand-in: the network is dead, and the runner looks at its servers to see that nothing arrived.
import { MIB, api, guard, pattern, rejection, report, sleep, suite, verdict } from './harness.js';

const { http } = api;

const expectCode = async (what, promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`${what}: ${error?.code ?? error?.name ?? 'it succeeded'}, expected ${code}`);
  return error;
};

/** What the server works out of a body: the sum of (position + 1) * byte, as text (in blocks, so that no number outgrows a double). */
function sumOf(bytes) {
  let sum = 0n;
  for (let start = 0; start < bytes.length; start += 65536) {
    const end = Math.min(start + 65536, bytes.length);
    let part = 0;
    for (let i = start; i < end; i += 1) part += (i + 1) * bytes[i];
    sum += BigInt(part);
  }
  return sum;
}

async function allowed(t, check) {
  const url = path => `http://127.0.0.1:${t.port}${path}`;
  const other = path => `http://127.0.0.1:${t.otherPort}${path}`;

  await check('http-a-request-and-its-answer', async () => {
    const hello = await http.request(url('/hello'));
    if (hello.status !== 200 || hello.statusText !== 'OK' || !hello.ok) throw new Error(`${hello.status} ${hello.statusText}`);
    if (hello.headers.get('x-test') !== 'a' || hello.url !== url('/hello') || hello.redirected) throw new Error('the head');
    if (await hello.text() !== 'hello') throw new Error('the body');

    const text = 'héllo — мир 🌍';
    const echoed = await http.request(url('/echo'), { method: 'post', body: text, headers: { 'X-One': '1' } });
    if (echoed.headers.get('x-method') !== 'POST' || echoed.headers.get('x-seen-one') !== '1') throw new Error('the method or the header');
    if (await echoed.text() !== text) throw new Error('the text did not come back');
    const every = Uint8Array.from({ length: 256 }, (_, index) => index);
    const bytes = await (await http.request(url('/echo'), { method: 'PUT', body: every })).bytes();
    if (bytes.length !== 256 || bytes.some((byte, index) => byte !== index)) throw new Error('the bytes changed');
    const json = await (await http.request(url('/json'))).json();
    if (json.ok !== true || json.list.length !== 3) throw new Error('json');

    const head = await http.request(url('/hello'), { method: 'HEAD' });
    if (head.status !== 200 || head.body !== null) throw new Error('a HEAD has a body');
    const nobody = await http.request(url('/nobody'));
    if (nobody.status !== 204 || nobody.body !== null) throw new Error('a 204 has a body');
    const missing = await http.request(url('/notfound'));
    if (missing.status !== 404 || missing.ok || await missing.text() !== 'no such thing') throw new Error('a 404 is an answer, not an error');
  });

  await check('http-a-big-body-goes-up-and-comes-down-as-a-stream', async () => {
    const data = pattern(t.size);
    const started = performance.now();
    let at = 0;
    const source = new ReadableStream({
      pull(controller) {
        if (at >= data.length) {
          controller.close();
          return;
        }
        controller.enqueue(data.slice(at, at + 256 * 1024));
        at += 256 * 1024;
      },
    });
    const sent = await http.request(url('/sum'), { method: 'POST', body: source });
    const expected = sumOf(data).toString();
    const got = await sent.text();
    if (got !== expected || sent.headers.get('x-length') !== String(t.size)) throw new Error(`the server worked out ${got} of ${sent.headers.get('x-length')} bytes, expected ${expected}`);

    const down = await http.request(url(`/big?size=${t.size}`));
    let seen = 0;
    const reader = down.body.getReader();
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      for (let i = 0; i < value.length; i += 1) {
        if (value[i] !== data[seen + i]) throw new Error(`byte ${seen + i} changed`);
      }
      seen += value.length;
    }
    if (seen !== t.size) throw new Error(`${seen} of ${t.size} bytes came`);
    return `${t.size / MIB} MiB each way in ${Math.round(performance.now() - started)} ms`;
  });

  await check('http-redirects-follow-and-the-scope-holds-for-every-hop', async () => {
    const followed = await http.request(url('/redirect?to=/hello'));
    if (!followed.redirected || followed.url !== url('/hello') || await followed.text() !== 'hello') throw new Error('follow');
    const manual = await http.request(url('/redirect?to=/hello'), { redirect: 'manual' });
    if (manual.status !== 302 || manual.headers.get('location') !== '/hello' || manual.redirected) throw new Error('manual');
    const away = await expectCode('a redirect out of the scope', http.request(url(`/redirect?to=${encodeURIComponent(other('/hello'))}`)), 'PERMISSION_DENIED');
    if (away.details?.permission !== 'net.http') throw new Error(JSON.stringify(away.details));
    await expectCode('another origin', http.request(other('/hello')), 'PERMISSION_DENIED');
    await expectCode('another host', http.request('https://example.com/'), 'PERMISSION_DENIED');
    await expectCode('not http', http.request('ftp://127.0.0.1/'), 'PERMISSION_DENIED');
    await expectCode('too many redirects', http.request(url('/loop')), 'NETWORK');
  });

  await check('http-time-and-the-network-fail-with-their-own-codes', async () => {
    const started = performance.now();
    await expectCode('a slow server', http.request(url('/slow'), { timeout: 300 }), 'TIMEOUT');
    if (performance.now() - started > 2500) throw new Error('the timeout did not cut it short');
    await expectCode('nothing listens', http.request(`http://127.0.0.1:${t.closed}/`, { timeout: 5000 }), 'NETWORK');
  });

  await check('http-an-abort-reaches-the-connection', async () => {
    const controller = new AbortController();
    const slow = rejection(http.request(url('/slow'), { signal: controller.signal }));
    await sleep(200);
    controller.abort();
    const error = await slow;
    if (error?.name !== 'AbortError') throw new Error(`${error?.name ?? 'it succeeded'}`);
    const big = await http.request(url('/big?size=268435456'));
    const reader = big.body.getReader();
    const first = await reader.read();
    if (first.done || first.value.length === 0) throw new Error('no first piece');
    await reader.cancel();
  });

  await check('http-a-download-fills-a-file-with-progress-and-an-abort-leaves-none', async () => {
    const progress = [];
    await http.download(url(`/big?size=${t.size}`), `${t.root}/big.bin`, { onProgress: item => progress.push(item) });
    const last = progress.at(-1);
    if (progress.length < 2 || last.received !== t.size || last.total !== t.size) throw new Error(JSON.stringify(progress.slice(-2)));
    if (progress.some((item, index) => index > 0 && item.received < progress[index - 1].received)) throw new Error('the progress went back');

    await expectCode('a failure of the server', http.download(url('/notfound'), `${t.root}/missing.bin`), 'NETWORK');
    await expectCode('outside fs.write', http.download(url('/hello'), `${t.root}/../outside.bin`), 'PERMISSION_DENIED');
    const controller = new AbortController();
    const stopped = http.download(url('/big?size=268435456'), `${t.root}/stopped.bin`, { signal: controller.signal, onProgress: () => controller.abort() });
    const error = await rejection(stopped);
    if (error?.name !== 'AbortError') throw new Error(`${error?.name ?? 'it succeeded'}`);
    await sleep(800);
  });
}

async function substituted(t, check) {
  await check('http-a-substituted-network-hangs-until-its-time-is-up', async () => {
    const started = performance.now();
    await expectCode('a request', http.request(`http://127.0.0.1:${t.port}/hello`, { timeout: 400 }), 'TIMEOUT');
    if (performance.now() - started < 350) throw new Error('it did not hang');
    await expectCode('a download', http.download(`http://127.0.0.1:${t.port}/big`, `${t.root}/never.bin`, { timeout: 300 }), 'TIMEOUT');
    await expectCode('a request with a body', http.request(`http://127.0.0.1:${t.port}/sum`, { method: 'POST', body: new Uint8Array(300 * 1024), timeout: 300 }), 'TIMEOUT');
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
