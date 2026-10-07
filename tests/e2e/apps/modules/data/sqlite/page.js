// sqlite scenario: databases in files through the real transport. In `real` mode the user allowed the
// scope: the page makes a database in it, works with it, and the runner opens the file itself afterwards.
// In `substituted` mode he chose a stand-in: the page finds its database as if it were in the folder,
// and the runner looks at the real folder.
import { api, guard, rejection, suite, verdict } from './harness.js';

const { sqlite } = api;

const sep = path => (path.includes('\\') ? '\\' : '/');
const join = (base, ...names) => [base.replace(/[\\/]+$/, ''), ...names].join(sep(base));

const expectCode = async (what, promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`${what}: ${error?.code ?? 'it succeeded'}, expected ${code}`);
  return error;
};

async function real(t, check) {
  const file = join(t.root, 'app.db');
  let db;

  await check('sqlite-schema-insert-and-query-with-every-kind-of-value', async () => {
    db = await sqlite.open(file);
    await db.exec(`CREATE TABLE items (
      id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, price REAL, big INTEGER, data BLOB, note TEXT
    )`);
    const first = await db.exec(
      'INSERT INTO items (name, price, big, data, note) VALUES (?, ?, ?, ?, ?)',
      ['héllo — мир 🌍', 2.5, 9007199254740993n, new Uint8Array([0, 1, 255, 128]), null],
    );
    if (first.changes !== 1 || first.lastInsertId !== 1) throw new Error(JSON.stringify(first));
    await db.exec('INSERT INTO items (name, price) VALUES (:name, :price)', { name: 'second', price: 10 });
    const rows = await db.query('SELECT * FROM items ORDER BY id');
    const [a, b] = rows;
    if (rows.length !== 2 || a.name !== 'héllo — мир 🌍' || a.price !== 2.5 || a.note !== null) throw new Error(JSON.stringify(rows));
    if (a.big !== 9007199254740993n) throw new Error(`big: ${String(a.big)} ${typeof a.big}`);
    if (!(a.data instanceof Uint8Array) || a.data.join() !== '0,1,255,128') throw new Error(`data: ${a.data}`);
    if (b.id !== 2 || b.name !== 'second' || b.price !== 10) throw new Error(JSON.stringify(b));
    const [typed] = await db.query('SELECT typeof(big) AS big, typeof(data) AS data FROM items WHERE id = 1');
    if (typed.big !== 'integer' || typed.data !== 'blob') throw new Error(JSON.stringify(typed));
  });

  await check('sqlite-a-transaction-commits-and-a-failing-one-rolls-back', async () => {
    await db.transaction(async tx => {
      await tx.exec('INSERT INTO items (name) VALUES (?)', ['in-transaction']);
      const [seen] = await tx.query('SELECT count(*) AS n FROM items');
      if (seen.n !== 3) throw new Error(`inside, ${seen.n} rows`);
    });
    const failure = await rejection(db.transaction(async tx => {
      await tx.exec('INSERT INTO items (name) VALUES (?)', ['never']);
      await tx.exec('INSERT INTO items (name) VALUES (?)', ['second']);
    }));
    if (failure?.code !== 'INVALID_ARGUMENT' || failure.details?.sqlite !== 'SQLITE_CONSTRAINT_UNIQUE') throw new Error(`the failure: ${failure?.code} ${JSON.stringify(failure?.details)}`);
    const [after] = await db.query("SELECT count(*) AS n, sum(name = 'never') AS never FROM items");
    if (after.n !== 3 || after.never) throw new Error(`after the rollback: ${JSON.stringify(after)}`);
    // What asks of the database during a transaction waits for it to end (and so is not awaited inside it).
    let late;
    await db.transaction(async tx => {
      late = db.query('SELECT count(*) AS n FROM items');
      await tx.exec('INSERT INTO items (name) VALUES (?)', ['four']);
      const [inside] = await tx.query('SELECT count(*) AS n FROM items');
      if (inside.n !== 4) throw new Error(`inside ${inside.n}`);
    });
    const [outside] = await late;
    if (outside.n !== 4) throw new Error(`outside ${outside.n}`);
  });

  await check('sqlite-a-prepared-statement-runs-with-other-parameters', async () => {
    const insert = await db.prepare('INSERT INTO items (name, price) VALUES (?, ?)');
    for (let index = 0; index < 20; index += 1) await insert.run([`p${index}`, index / 4]);
    const find = await db.prepare('SELECT name FROM items WHERE price >= ? ORDER BY price, name LIMIT 3');
    const names = rows => rows.map(row => row.name).join();
    if (names(await find.all([4.75])) !== 'p19,second') throw new Error(names(await find.all([4.75])));
    if (names(await find.all([4.0])) !== 'p16,p17,p18') throw new Error(names(await find.all([4.0])));
    await insert.finalize();
    await expectCode('a finalized statement', insert.run(['x', 1]), 'NOT_FOUND');
    await find.finalize();
  });

  await check('sqlite-a-hundred-thousand-rows-pass-in-order-through-iterate', async () => {
    await db.exec('CREATE TABLE many (n INTEGER, text TEXT)');
    await db.exec(`WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < ${t.many})
      INSERT INTO many SELECT x, 'row ' || x FROM c`);
    const started = performance.now();
    let expected = 1;
    for await (const row of db.iterate('SELECT n, text FROM many ORDER BY n')) {
      if (row.n !== expected || row.text !== `row ${expected}`) throw new Error(`row ${expected}: ${JSON.stringify(row)}`);
      expected += 1;
    }
    if (expected !== t.many + 1) throw new Error(`${expected - 1} rows, expected ${t.many}`);
    const all = await expectCode('too many for one answer', db.query('SELECT n FROM many, many AS more LIMIT 200000'), 'INVALID_ARGUMENT');
    void all;
    return `${t.many} rows in ${Math.round(performance.now() - started)} ms`;
  });

  await check('sqlite-the-connection-is-busy-while-iterating-and-free-after', async () => {
    let seen = 0;
    for await (const row of db.iterate('SELECT n FROM many ORDER BY n LIMIT 10')) {
      void row;
      seen += 1;
      await expectCode('a query inside the loop', db.query('SELECT 1'), 'BUSY');
    }
    if (seen !== 10) throw new Error(`${seen} rows`);
    for await (const row of db.iterate('SELECT n FROM many')) {
      void row;
      break;
    }
    const [one] = await db.query('SELECT 1 AS one');
    if (one.one !== 1) throw new Error('the connection is not free after leaving the loop');
  });

  await check('sqlite-errors-say-what-failed', async () => {
    const syntax = await expectCode('syntax', db.query('SELEC 1'), 'INVALID_ARGUMENT');
    if (!/syntax/.test(syntax.message)) throw new Error(syntax.message);
    await expectCode('no such table', db.query('SELECT * FROM nothing'), 'INVALID_ARGUMENT');
    await expectCode('a query that is no query', db.query('INSERT INTO items (name) VALUES (1)'), 'INVALID_ARGUMENT');
    await expectCode('a list as a parameter', db.exec('INSERT INTO items (name) VALUES (?)', [[1]]), 'INVALID_ARGUMENT');
    await expectCode('one too few parameters', db.exec('INSERT INTO items (name, price) VALUES (?, ?)', ['x']), 'INVALID_ARGUMENT');
  });

  await check('sqlite-sql-cannot-reach-another-file-or-load-code', async () => {
    const target = join(t.outside, 'other.db').replaceAll("'", "''");
    await expectCode('attach', db.exec(`ATTACH DATABASE '${target}' AS other`), 'PERMISSION_DENIED');
    const vacuum = await rejection(db.exec(`VACUUM INTO '${target}'`));
    if (!vacuum) throw new Error('vacuum into a file outside the scope was accepted');
    const extension = await rejection(db.query("SELECT load_extension('nothing')"));
    if (!extension) throw new Error('an extension was loaded');
    await db.exec('VACUUM');
  });

  await check('sqlite-a-database-closes-and-is-found-again-in-its-file', async () => {
    await db.close();
    await expectCode('a closed database', db.query('SELECT 1'), 'NOT_FOUND');
    const again = await sqlite.open(file, { readonly: true });
    const [count] = await again.query('SELECT count(*) AS n FROM items');
    if (count.n !== 24) throw new Error(`${count.n} items`);
    await expectCode('writing a database opened to read', again.exec('DELETE FROM items'), 'PERMISSION_DENIED');
    await again.close();
    await expectCode('a database that is not there', sqlite.open(join(t.root, 'missing.db'), { readonly: true }), 'NOT_FOUND');
    await expectCode('outside the scope', sqlite.open(join(t.outside, 'o.db')), 'PERMISSION_DENIED');
  });
}

async function substituted(t, check) {
  await check('sqlite-a-stand-in-keeps-the-database-and-the-folder-stays-empty', async () => {
    const file = join(t.root, 'app.db');
    const db = await sqlite.open(file);
    await db.exec('CREATE TABLE t (a INTEGER)');
    await db.exec('INSERT INTO t VALUES (1), (2), (3)');
    await db.close();
    const again = await sqlite.open(file, { readonly: true });
    const [count] = await again.query('SELECT count(*) AS n FROM t');
    if (count.n !== 3) throw new Error(`${count.n} rows`);
    await again.close();
    await expectCode('outside', sqlite.open(join(t.outside, 'o.db')), 'PERMISSION_DENIED');
  });
}

async function main() {
  const t = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  if (t.mode === 'substituted') await substituted(t, check);
  else await real(t, check);
  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
