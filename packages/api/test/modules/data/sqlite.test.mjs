// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, SqliteDatabase, SqliteStatement, sqlite } from '../../../src/index.ts';
import { endFrame, installRuntime, join, jsonFrame } from '../../fake-runtime.mjs';

const replies = new Map();
const streams = new Map();
/** Every call of the commands of sqlite, in the order they reached the runtime. */
const order = [];
const runtime = installRuntime({
  handler(request) {
    const stream = /^native:\/\/stream\/(\d+)$/.exec(request.url);
    if (stream) return streams.get(Number(stream[1])) ?? { status: 404, json: { code: 'NOT_FOUND', message: 'stream not found' } };
    const command = request.url.replace('native://call/', '');
    if (command.startsWith('sqlite.')) {
      const args = runtime.argsOf(request);
      order.push(`${command}:${args?.sql ?? args?.statement ?? ''}`);
      if (args?.sql === 'BAD') {
        return { status: 400, json: { code: 'INVALID_ARGUMENT', message: 'constraint failed' } };
      }
    }
    return replies.get(command) ?? { json: null };
  },
});

async function opened() {
  replies.set('sqlite.open', { json: { db: 7 } });
  replies.set('sqlite.exec', { json: { changes: 1, lastInsertId: 5 } });
  replies.set('sqlite.query', { json: [] });
  const db = await sqlite.open('/a.db');
  order.length = 0;
  return db;
}

const argsOf = command => runtime.argsOf(runtime.calls(command).at(-1));

test('open sends the flags and not the signal, and gives a database that sends its id', async () => {
  replies.set('sqlite.open', { json: { db: 7 } });
  const controller = new AbortController();
  const db = await sqlite.open('/a.db', { readonly: true, create: false, signal: controller.signal });
  assert.ok(db instanceof SqliteDatabase);
  assert.equal(db.id, 7);
  assert.deepEqual(argsOf('sqlite.open'), { path: '/a.db', readonly: true, create: false });
  replies.set('sqlite.query', { json: [] });
  await db.query('SELECT 1');
  assert.deepEqual(argsOf('sqlite.query'), { db: 7, sql: 'SELECT 1' });
  replies.set('sqlite.close', { json: null });
  await db.close();
  assert.deepEqual(argsOf('sqlite.close'), { db: 7 });
  assert.equal(typeof SqliteDatabase.prototype[Symbol.asyncDispose], 'function');
  assert.equal(typeof SqliteStatement.prototype[Symbol.asyncDispose], 'function');
});

test('parameters go as JSON with tags for what JSON cannot hold; nothing else is let through', async () => {
  const db = await opened();
  await db.exec('INSERT', [1, 'two', null, true, 3n, 9007199254740993n, new Uint8Array([0, 1, 255]), Infinity, -Infinity, 1.5]);
  assert.deepEqual(argsOf('sqlite.exec').params, [
    1, 'two', null, true, { $int: '3' }, { $int: '9007199254740993' }, { $blob: 'AAH/' }, { $real: 'Infinity' }, { $real: '-Infinity' }, 1.5,
  ]);
  await db.exec('INSERT', { name: 'x', ':other': 2n });
  assert.deepEqual(argsOf('sqlite.exec').params, { name: 'x', ':other': { $int: '2' } });
  await db.exec('CREATE TABLE t (a)');
  assert.equal('params' in argsOf('sqlite.exec'), false, 'no parameters, none sent');

  const before = runtime.calls('sqlite.exec').length;
  for (const bad of [undefined, NaN, {}, [1], () => 1, Symbol('s'), new Date()]) {
    await assert.rejects(db.exec('INSERT', [bad]), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT', String(bad));
  }
  await assert.rejects(db.exec('INSERT', { a: undefined }), { code: 'INVALID_ARGUMENT' });
  assert.equal(runtime.calls('sqlite.exec').length, before, 'refused before anything was sent');
});

test('what the runtime gives is turned into what a program has: bigint, bytes, infinity', async () => {
  const db = await opened();
  replies.set('sqlite.exec', { json: { changes: 2, lastInsertId: { $int: '9007199254740993' } } });
  assert.deepEqual(await db.exec('INSERT'), { changes: 2, lastInsertId: 9007199254740993n });
  replies.set('sqlite.exec', { json: { changes: 1, lastInsertId: 12 } });
  assert.deepEqual(await db.exec('INSERT'), { changes: 1, lastInsertId: 12 });
  replies.set('sqlite.query', {
    json: [{ a: 1, b: 'text', c: null, d: { $int: '-9223372036854775808' }, e: { $blob: 'AAH/' }, f: { $real: '-Infinity' }, g: 2.5 }],
  });
  const [row] = await db.query('SELECT');
  assert.equal(row.a, 1);
  assert.equal(row.b, 'text');
  assert.equal(row.c, null);
  assert.equal(row.d, -9223372036854775808n);
  assert.ok(row.e instanceof Uint8Array);
  assert.deepEqual([...row.e], [0, 1, 255]);
  assert.equal(row.f, -Infinity);
  assert.equal(row.g, 2.5);
});

test('a failure of the runtime reaches the caller as an AlefError with its details', async () => {
  const db = await opened();
  replies.set('sqlite.query', {
    status: 400,
    json: { code: 'INVALID_ARGUMENT', message: 'UNIQUE constraint failed: t.a', details: { sqlite: 'SQLITE_CONSTRAINT_UNIQUE' } },
  });
  await assert.rejects(db.query('SELECT'), error => error instanceof AlefError
    && error.code === 'INVALID_ARGUMENT' && error.details.sqlite === 'SQLITE_CONSTRAINT_UNIQUE');
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(db.query('SELECT', undefined, { signal: controller.signal }), { name: 'AbortError' });
});

test('iterate yields the rows of every frame, and the connection answers nothing else meanwhile', async () => {
  const db = await opened();
  replies.set('sqlite.iterate', { json: { stream: 41 } });
  streams.set(41, { chunks: [join(jsonFrame([{ n: 1 }, { n: { $int: '9007199254740993' } }]), jsonFrame([{ n: 3 }]), endFrame())] });
  const seen = [];
  for await (const row of db.iterate('SELECT n FROM t')) {
    seen.push(row.n);
    await assert.rejects(db.query('SELECT 1'), { code: 'BUSY' });
    await assert.rejects(db.exec('DELETE FROM t'), { code: 'BUSY' });
    await assert.rejects(async () => {
      for await (const other of db.iterate('SELECT 2')) void other;
    }, { code: 'BUSY' });
  }
  assert.deepEqual(seen, [1, 9007199254740993n, 3]);
  await db.query('SELECT 1');

  streams.set(41, { chunks: [join(jsonFrame([{ n: 1 }, { n: 2 }]), endFrame())] });
  for await (const row of db.iterate('SELECT n FROM t')) {
    void row;
    break;
  }
  await db.query('SELECT 1');
  assert.deepEqual(argsOf('sqlite.iterate'), { db: 7, sql: 'SELECT n FROM t' });
});

test('a statement is prepared once and run by its id', async () => {
  const db = await opened();
  replies.set('sqlite.prepare', { json: { statement: 11 } });
  const statement = await db.prepare('INSERT INTO t VALUES (?)');
  assert.ok(statement instanceof SqliteStatement);
  assert.deepEqual(argsOf('sqlite.prepare'), { db: 7, sql: 'INSERT INTO t VALUES (?)' });
  assert.deepEqual(await statement.run([5n]), { changes: 1, lastInsertId: 5 });
  assert.deepEqual(argsOf('sqlite.exec'), { statement: 11, params: [{ $int: '5' }] });
  replies.set('sqlite.query', { json: [{ a: 1 }] });
  assert.deepEqual(await statement.all([1]), [{ a: 1 }]);
  assert.deepEqual(argsOf('sqlite.query'), { statement: 11, params: [1] });
  replies.set('sqlite.iterate', { json: { stream: 42 } });
  streams.set(42, { chunks: [join(jsonFrame([{ a: 1 }]), endFrame())] });
  const rows = [];
  for await (const row of statement.iterate()) rows.push(row);
  assert.deepEqual(rows, [{ a: 1 }]);
  replies.set('sqlite.finalize', { json: null });
  await statement.finalize();
  assert.deepEqual(argsOf('sqlite.finalize'), { statement: 11 });
});

test('a transaction is BEGIN, the work and COMMIT, and what else asks waits for it', async () => {
  const db = await opened();
  let outside;
  const result = await db.transaction(async tx => {
    await tx.exec('INSERT 1');
    outside = db.exec('INSERT outside');
    const rows = await tx.query('SELECT inside');
    replies.set('sqlite.prepare', { json: { statement: 12 } });
    const statement = await tx.prepare('INSERT 2');
    await statement.run([1]);
    return rows.length;
  });
  assert.equal(result, 0);
  await outside;
  assert.deepEqual(order, [
    'sqlite.exec:BEGIN', 'sqlite.exec:INSERT 1', 'sqlite.query:SELECT inside', 'sqlite.prepare:INSERT 2', 'sqlite.exec:12',
    'sqlite.exec:COMMIT', 'sqlite.exec:INSERT outside',
  ]);
});

test('a transaction whose work throws is rolled back, and the failure of the work is the news', async () => {
  const db = await opened();
  const failure = new Error('the work failed');
  await assert.rejects(db.transaction(async tx => {
    await tx.exec('INSERT 1');
    throw failure;
  }), error => error === failure);
  assert.deepEqual(order, ['sqlite.exec:BEGIN', 'sqlite.exec:INSERT 1', 'sqlite.exec:ROLLBACK']);

  order.length = 0;
  await assert.rejects(db.transaction(async tx => {
    await tx.exec('BAD');
  }), error => error instanceof AlefError && error.message === 'constraint failed');
  assert.deepEqual(order, ['sqlite.exec:BEGIN', 'sqlite.exec:BAD', 'sqlite.exec:ROLLBACK']);
  await db.exec('AFTER');
  assert.equal(order.at(-1), 'sqlite.exec:AFTER', 'the line is free again');
});
