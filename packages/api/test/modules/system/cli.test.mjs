// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, ChildProcess, cli } from '../../../src/index.ts';
import { binaryFrame, endFrame, installRuntime, join } from '../../fake-runtime.mjs';

const replies = new Map();
const streams = new Map();
/** Calls the runtime keeps waiting until their signal is aborted (when they carry one). */
const hold = new Set();
let onHold = () => {};
/** A call that never reaches the runtime (it has no signal to hold on) must not hang the test. */
const soon = promise => Promise.race([promise, new Promise(resolve => setTimeout(resolve, 500))]);
const runtime = installRuntime({
  handler(request) {
    const stream = /^native:\/\/stream\/(\d+)$/.exec(request.url);
    if (stream) return streams.get(Number(stream[1])) ?? { status: 404, json: { code: 'NOT_FOUND', message: 'stream not found' } };
    const name = request.url.replace('native://call/', '');
    if (hold.has(name) && request.signal) {
      onHold();
      return new Promise((_, reject) => request.signal.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')), { once: true }));
    }
    return replies.get(name) ?? { json: null };
  },
});

test('run transports declared params, options and binary or string stdin without a shell', async () => {
  const result = { code: 3, signal: null, stdout: 'out', stderr: 'err' };
  replies.set('cli.run', { json: result });
  assert.deepEqual(await cli.run('hello', { value: 'a b' }, {
    cwd: '/w', env: { A: 'b' }, timeout: 90, input: 'é', shell: true,
  }), result);
  assert.deepEqual(argsOf('cli.run'), { name: 'hello', params: { value: 'a b' }, cwd: '/w', env: [['A', 'b']], timeoutMs: 90 });
  assert.deepEqual(runtime.calls('cli.run').at(-1).body, encode('é'));
  await cli.run('hello');
  assert.deepEqual(argsOf('cli.run'), { name: 'hello' });
  for (const input of [new Uint8Array(192 * 1024), 'é'.repeat(96 * 1024)]) {
    await cli.run('hello', {}, { input });
    assert.equal(runtime.calls('cli.run').at(-1).body.length, 192 * 1024);
  }
  const before = runtime.calls('cli.run').length;
  for (const input of [new Uint8Array(192 * 1024 + 1), 'é'.repeat(96 * 1024 + 1)]) {
    await assert.rejects(cli.run('hello', undefined, { input }), { code: 'INVALID_ARGUMENT' });
  }
  assert.equal(runtime.calls('cli.run').length, before);
});

test('run and start validate names and string params before transport', async () => {
  for (const method of ['run', 'start']) {
    const before = runtime.calls(`cli.${method}`).length;
    for (const name of ['', 'a\0b', null, undefined, 42]) {
      await assert.rejects(cli[method](name), { code: 'INVALID_ARGUMENT' });
    }
    for (const params of [null, [], 42, 'x', { a: 1 }, { a: null }, { a: 'b\0c' }, { ['a\0b']: 'c' }]) {
      await assert.rejects(cli[method]('hello', params), { code: 'INVALID_ARGUMENT' });
    }
    assert.equal(runtime.calls(`cli.${method}`).length, before);
  }
});

test('start returns ChildProcess with stdin, output, wait and kill using the opened resource', async () => {
  replies.set('cli.start', { json: { process: 10, pid: 4245, stdin: 71, stdout: 72, stderr: null } });
  replies.set('cli.wait', { json: { code: 0, signal: null } });
  streams.set(71, endless());
  streams.set(72, { chunks: [join(binaryFrame(encode('declared')), endFrame())] });
  const child = await cli.start('hello', { value: 'literal' }, { cwd: '/w', env: { A: 'b' }, stdin: 'pipe', stdout: 'pipe', stderr: 'ignore' });
  assert.ok(child instanceof ChildProcess);
  assert.equal(child.pid, 4245);
  assert.equal(child.stderr, null);
  assert.deepEqual(argsOf('cli.start'), { name: 'hello', params: { value: 'literal' }, cwd: '/w', env: [['A', 'b']], stdin: 'pipe', stdout: 'pipe', stderr: 'ignore' });
  assert.deepEqual(await readAll(child.stdout), ['declared']);
  const writer = child.stdin.getWriter();
  await writer.write(encode('in'));
  assert.deepEqual(argsOf('runtime.stream.write'), { id: 71 });
  assert.equal(decode(runtime.calls('runtime.stream.write').at(-1).body), 'in');
  await writer.close();
  writer.releaseLock();
  await child.kill('SIGKILL');
  assert.deepEqual(argsOf('cli.kill'), { process: 10, signal: 'SIGKILL' });
  assert.deepEqual(await child.wait(), { code: 0, signal: null });
  assert.deepEqual(argsOf('cli.wait'), { process: 10 });
  replies.set('cli.start', { json: { process: 11, pid: 4246, stdin: null, stdout: null, stderr: null } });
  const ignored = await cli.start('hello');
  assert.deepEqual(argsOf('cli.start'), { name: 'hello' });
  assert.equal(ignored.stdin, null);
  assert.equal(ignored.stdout, null);
  assert.equal(ignored.stderr, null);
});

for (const method of ['run', 'start']) {
  test(`abort during ${method} reaches the runtime`, async () => {
    const command = `cli.${method}`;
    hold.add(command);
    const controller = new AbortController();
    const reached = new Promise(resolve => { onHold = resolve; });
    const outcome = cli[method]('hello', undefined, { signal: controller.signal }).then(() => 'success', error => error?.name);
    try {
      await soon(reached);
      assert.equal(runtime.calls(command).at(-1).signal, controller.signal);
      controller.abort();
      assert.equal(await soon(outcome), 'AbortError');
    } finally {
      controller.abort();
      hold.delete(command);
      onHold = () => {};
    }
  });

  test(`${method} propagates native errors`, async () => {
    replies.set(`cli.${method}`, { status: 504, json: { code: 'TIMEOUT', message: 'substituted', details: { name: 'hello' } } });
    await assert.rejects(cli[method]('hello'), error => {
      assert.ok(error instanceof AlefError);
      assert.equal(error.code, 'TIMEOUT');
      assert.equal(error.status, 504);
      assert.deepEqual(error.details, { name: 'hello' });
      return true;
    });
  });
}

const encode = text => new TextEncoder().encode(text);
const decode = bytes => new TextDecoder().decode(bytes);
const argsOf = command => runtime.argsOf(runtime.calls(command).at(-1));
const endless = () => ({ chunks: [endFrame()] });

async function readAll(stream) {
  const pieces = [];
  const reader = stream.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) return pieces.map(decode);
    pieces.push(value);
  }
}

test('exec passes the command line, the shell, the cwd, the env pairs, the timeout and the body', async () => {
  replies.set('cli.exec', { json: { code: 3, signal: null, stdout: 'out', stderr: 'err' } });
  streams.set(11, { chunks: [join(binaryFrame(encode('out')), endFrame())] });
  const result = await cli.exec('node -v', { shell: true, cwd: '/w', env: { A: 'b' }, timeout: 900, input: 'go' });
  assert.deepEqual(result, { code: 3, signal: null, stdout: 'out', stderr: 'err' });
  assert.deepEqual(argsOf('cli.exec'), { commandLine: 'node -v', shell: true, cwd: '/w', env: [['A', 'b']], timeoutMs: 900 });
  assert.equal(decode(runtime.calls('cli.exec').at(-1).body), 'go', 'the input travels in the body');

  await cli.exec('node -v');
  assert.deepEqual(argsOf('cli.exec'), { commandLine: 'node -v' }, 'the omitted options do not appear');
  await cli.exec('node -v', { shell: 'powershell' });
  assert.deepEqual(argsOf('cli.exec'), { commandLine: 'node -v', shell: 'powershell' });
});

test('exec validates the command line and the input before the runtime hears of it', async () => {
  const before = runtime.calls('cli.exec').length;
  for (const bad of ['', '   ', 42, null, undefined]) {
    await assert.rejects(cli.exec(bad), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  }
  assert.equal(runtime.calls('cli.exec').length, before, 'nothing was asked of the runtime');

  await assert.rejects(
    cli.exec('x', { input: 'y'.repeat(192 * 1024 + 1) }),
    error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT',
  );
  assert.equal(runtime.calls('cli.exec').length, before, 'the too big input never reached the runtime');

  replies.set('cli.exec', { json: { code: 0, signal: null, stdout: '', stderr: '' } });
  await cli.exec('x', { input: 'y'.repeat(192 * 1024) });
  assert.equal(runtime.calls('cli.exec').length, before + 1, 'exactly 192 KiB does fit a call');
});

test('an abort during exec is forwarded to the waiting runtime', async () => {
  hold.add('cli.exec');
  const controller = new AbortController();
  const reached = new Promise(resolve => { onHold = resolve; });
  const before = runtime.calls('cli.exec').length;
  const outcome = cli.exec('x', { signal: controller.signal }).then(() => 'it went through', error => error?.name);
  try {
    await soon(reached);
    assert.equal(runtime.calls('cli.exec').length, before + 1);
    const request = runtime.calls('cli.exec').at(-1);
    assert.equal(request.signal, controller.signal);
    assert.equal(request.signal.aborted, false, 'exec is in flight before abort');
    controller.abort();
    assert.equal(request.signal.aborted, true);
    assert.equal(await soon(outcome), 'AbortError');
  } finally {
    controller.abort();
    hold.delete('cli.exec');
    onHold = () => {};
  }
});

test('an abort during spawn is forwarded to the waiting runtime', async () => {
  hold.add('cli.spawn');
  const controller = new AbortController();
  const reached = new Promise(resolve => { onHold = resolve; });
  const before = runtime.calls('cli.spawn').length;
  const outcome = cli.spawn('node', [], { signal: controller.signal }).then(() => 'it went through', error => error?.name);
  try {
    await soon(reached);
    assert.equal(runtime.calls('cli.spawn').length, before + 1);
    const request = runtime.calls('cli.spawn').at(-1);
    assert.equal(request.signal, controller.signal);
    controller.abort();
    assert.equal(await soon(outcome), 'AbortError');
  } finally {
    controller.abort();
    hold.delete('cli.spawn');
    onHold = () => {};
  }
});

test('spawn builds the ChildProcess streams and passes the arguments', async () => {
  replies.set('cli.spawn', { json: { process: 7, pid: 4242, stdin: 71, stdout: 72, stderr: null } });
  streams.set(71, endless());
  streams.set(72, { chunks: [join(binaryFrame(encode('hi')), endFrame())] });
  const child = await cli.spawn('node', ['-v'], { cwd: '/w', env: { A: 'b' }, stdout: 'pipe' });
  assert.ok(child instanceof ChildProcess);
  assert.deepEqual(argsOf('cli.spawn'), { program: 'node', args: ['-v'], cwd: '/w', env: [['A', 'b']], stdout: 'pipe' });
  assert.equal(child.pid, 4242);
  assert.equal(child.stderr, null);
  assert.deepEqual(await readAll(child.stdout), ['hi']);
});

test('what is written goes to the stdin stream, and the end of the writable ends it', async () => {
  replies.set('cli.spawn', { json: { process: 7, pid: 4242, stdin: 71, stdout: null, stderr: null } });
  streams.set(71, endless());
  const child = await cli.spawn('node', []);
  const writer = child.stdin.getWriter();
  const before = runtime.calls('runtime.stream.write').length;
  await writer.write(encode('abc'));
  const written = runtime.calls('runtime.stream.write').slice(before);
  assert.equal(written.length, 1);
  assert.deepEqual(runtime.argsOf(written[0]), { id: 71 });
  assert.equal(decode(written[0].body), 'abc');
  const ends = runtime.calls('runtime.stream.end').length;
  await writer.close();
  assert.equal(runtime.calls('runtime.stream.end').length, ends + 1);
  assert.deepEqual(runtime.argsOf(runtime.calls('runtime.stream.end').at(-1)), { id: 71 });
});

test('wait and kill name the process', async () => {
  replies.set('cli.spawn', { json: { process: 7, pid: 4242, stdin: null, stdout: null, stderr: null } });
  replies.set('cli.wait', { json: { code: 0, signal: null } });
  const child = await cli.spawn('node', []);
  assert.deepEqual(await child.wait(), { code: 0, signal: null });
  assert.deepEqual(argsOf('cli.wait'), { process: 7 });
  await child.kill();
  assert.deepEqual(argsOf('cli.kill'), { process: 7 });
  await child.kill('SIGTERM');
  assert.deepEqual(argsOf('cli.kill'), { process: 7, signal: 'SIGTERM' });
});

test('ignored pipes are null and do not open runtime streams', async () => {
  replies.set('cli.spawn', { json: { process: 8, pid: 4243, stdin: null, stdout: null, stderr: null } });
  const before = runtime.requests.filter(request => request.url.startsWith('native://stream/')).length;
  const child = await cli.spawn('node', [], { stdin: 'ignore', stdout: 'ignore', stderr: 'ignore' });
  assert.deepEqual(argsOf('cli.spawn'), { program: 'node', args: [], stdin: 'ignore', stdout: 'ignore', stderr: 'ignore' });
  assert.equal(child.stdin, null);
  assert.equal(child.stdout, null);
  assert.equal(child.stderr, null);
  assert.equal(runtime.requests.filter(request => request.url.startsWith('native://stream/')).length, before);
});

test('wait is consumed once by the runtime and subsequent errors propagate', async () => {
  replies.set('cli.spawn', { json: { process: 9, pid: 4244, stdin: null, stdout: null, stderr: null } });
  const child = await cli.spawn('node', []);
  const before = runtime.calls('cli.wait').length;
  replies.set('cli.wait', { json: { code: 2, signal: null } });
  assert.deepEqual(await child.wait(), { code: 2, signal: null });
  const failure = { code: 'NOT_FOUND', message: 'process already waited', details: { process: 9 } };
  replies.set('cli.wait', { status: 404, json: failure });
  await assert.rejects(child.wait(), error => {
    assert.ok(error instanceof AlefError);
    assert.equal(error.code, failure.code);
    assert.equal(error.message, failure.message);
    assert.equal(error.status, 404);
    assert.deepEqual(error.details, failure.details);
    return true;
  });
  assert.equal(runtime.calls('cli.wait').length, before + 2, 'wait results are not cached by JS');
  assert.deepEqual(argsOf('cli.wait'), { process: 9 });
});

test('spawn needs a program, and asks nothing of the runtime otherwise', async () => {
  const calls = runtime.calls('cli.spawn').length;
  await assert.rejects(cli.spawn(''), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  assert.equal(runtime.calls('cli.spawn').length, calls, 'nothing was asked of the runtime');
});
