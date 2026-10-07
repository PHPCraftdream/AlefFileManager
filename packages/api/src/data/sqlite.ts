// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError } from '../core/errors.ts';
import { openReadable } from '../core/stream.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from '../desktop/app.ts';

/** What SQLite keeps and a program can name: NULL, INTEGER (`number`, or `bigint` above 2^53), REAL, TEXT, BLOB. */
export type SqlValue = null | number | bigint | string | boolean | Uint8Array;

/** Parameters by position (`?`) or by name (`:name`, `@name`, `$name`; a name without a prefix is `:name`). */
export type SqlParams = SqlValue[] | Record<string, SqlValue>;

/** What a statement that changes things reports. */
export interface ExecResult {
  /** Rows changed by the last statement that changed any (0 when none did). */
  changes: number;
  /** The rowid of the last INSERT on the connection. */
  lastInsertId: number | bigint;
}

export interface SqliteOpenOptions extends Cancelable {
  /** Open to read only: needs only the right to read. */
  readonly?: boolean;
  /** Make the file when it is not there (the default; not for `readonly`). */
  create?: boolean;
}

const CHUNK = 0x8000;

function encodeBase64(bytes: Uint8Array): string {
  let binary = '';
  for (let at = 0; at < bytes.length; at += CHUNK) binary += String.fromCharCode(...bytes.subarray(at, at + CHUNK));
  return btoa(binary);
}

function decodeBase64(text: string): Uint8Array {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let at = 0; at < binary.length; at += 1) bytes[at] = binary.charCodeAt(at);
  return bytes;
}

/** A value of a parameter, as the runtime takes it (JSON, and tags for what JSON cannot hold). */
function toWire(value: unknown): unknown {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return value;
  if (typeof value === 'number') {
    if (Number.isNaN(value)) throw new AlefError('INVALID_ARGUMENT', 'NaN is not a SQL value');
    if (value === Infinity) return { $real: 'Infinity' };
    if (value === -Infinity) return { $real: '-Infinity' };
    return value;
  }
  if (typeof value === 'bigint') return { $int: value.toString() };
  if (value instanceof Uint8Array) return { $blob: encodeBase64(value) };
  throw new AlefError('INVALID_ARGUMENT', `${value === undefined ? 'undefined' : typeof value} is not a SQL value: use null for nothing`);
}

function paramsToWire(params: SqlParams | undefined): unknown {
  if (params === undefined) return undefined;
  if (Array.isArray(params)) return params.map(toWire);
  return Object.fromEntries(Object.entries(params).map(([name, value]) => [name, toWire(value)]));
}

/** A value the runtime gives, as a program has it. */
function fromWire(value: unknown): unknown {
  if (value !== null && typeof value === 'object') {
    const tag = value as Record<string, string>;
    if (typeof tag.$int === 'string') return BigInt(tag.$int);
    if (typeof tag.$blob === 'string') return decodeBase64(tag.$blob);
    if (typeof tag.$real === 'string') return tag.$real === '-Infinity' ? -Infinity : Infinity;
  }
  return value;
}

const rowFromWire = <T>(row: Record<string, unknown>): T =>
  Object.fromEntries(Object.entries(row).map(([name, value]) => [name, fromWire(value)])) as T;

/** What the commands of a database share: the id, the line of transactions and whether it is being iterated. */
class Line {
  readonly db: number;
  iterating = false;
  private tail: Promise<void> = Promise.resolve();

  constructor(db: number) {
    this.db = db;
  }

  /** Takes the next turn: what asks while a transaction is running waits until it is over. */
  async inTurn<T>(work: () => Promise<T>): Promise<T> {
    const turn = this.tail;
    let release!: () => void;
    this.tail = new Promise<void>(resolve => {
      release = resolve;
    });
    await turn;
    try {
      return await work();
    } finally {
      release();
    }
  }

  /** `work` in its turn, unless it is the transaction's own and the turn is already its. */
  run<T>(queued: boolean, work: () => Promise<T>): Promise<T> {
    return queued ? this.inTurn(work) : work();
  }

  /** A connection that is iterated answers nothing else until the loop is over. */
  idle(): void {
    if (this.iterating) {
      throw new AlefError('BUSY', 'the database is being iterated: finish the loop first');
    }
  }
}

interface Target {
  db?: number;
  sql?: string;
  statement?: number;
}

async function execute(line: Line, target: Target, params: SqlParams | undefined, options: Cancelable): Promise<ExecResult> {
  line.idle();
  const done = await call<{ changes: number; lastInsertId: unknown }>(
    'sqlite.exec',
    { ...target, params: paramsToWire(params) },
    options,
  );
  return { changes: done.changes, lastInsertId: fromWire(done.lastInsertId) as number | bigint };
}

async function all<T>(line: Line, target: Target, params: SqlParams | undefined, options: Cancelable): Promise<T[]> {
  line.idle();
  const rows = await call<Record<string, unknown>[]>('sqlite.query', { ...target, params: paramsToWire(params) }, options);
  return rows.map(row => rowFromWire<T>(row));
}

async function* rowsOf<T>(
  line: Line,
  queued: boolean,
  target: Target,
  params: SqlParams | undefined,
  options: Cancelable,
): AsyncGenerator<T> {
  // The loop takes its place after a transaction that is running and then keeps no turn: what asks
  // meanwhile is told `BUSY`, and the body of the loop may ask nothing of this connection.
  await line.run(queued, async () => {
    line.idle();
    line.iterating = true;
  });
  try {
    const { stream } = await call<{ stream: number }>('sqlite.iterate', { ...target, params: paramsToWire(params) }, options);
    for await (const frame of await openReadable(stream, options)) {
      if (frame.kind === 'json') for (const row of frame.value as Record<string, unknown>[]) yield rowFromWire<T>(row);
    }
  } finally {
    line.iterating = false;
  }
}

/** A statement that was prepared: parsed once, run as often as needed. */
export class SqliteStatement {
  readonly id: number;
  private readonly line: Line;
  private readonly queued: boolean;

  /** @internal */
  constructor(line: Line, id: number, queued: boolean) {
    this.line = line;
    this.id = id;
    this.queued = queued;
  }

  /** Runs a statement that changes things. */
  run(params?: SqlParams, options: Cancelable = {}): Promise<ExecResult> {
    return this.line.run(this.queued, () => execute(this.line, { statement: this.id }, params, options));
  }

  /** Runs a query and gives all its rows (up to 100000; more is for `iterate`). */
  all<T = Record<string, unknown>>(params?: SqlParams, options: Cancelable = {}): Promise<T[]> {
    return this.line.run(this.queued, () => all<T>(this.line, { statement: this.id }, params, options));
  }

  /** Runs a query and gives its rows one by one, with the memory of a few hundred of them. */
  iterate<T = Record<string, unknown>>(params?: SqlParams, options: Cancelable = {}): AsyncGenerator<T> {
    return rowsOf<T>(this.line, this.queued, { statement: this.id }, params, options);
  }

  finalize(options: Cancelable = {}): Promise<void> {
    return call<void>('sqlite.finalize', { statement: this.id }, options);
  }
}

const dispose = (Symbol as unknown as { asyncDispose?: symbol }).asyncDispose;
if (typeof dispose === 'symbol') {
  Object.defineProperty(SqliteStatement.prototype, dispose, {
    value(this: SqliteStatement): Promise<void> {
      return this.finalize();
    },
  });
}

/** What a transaction can do: the commands of the database, inside the transaction. */
export class SqliteTransaction {
  private readonly line: Line;

  /** @internal */
  constructor(line: Line) {
    this.line = line;
  }

  exec(sql: string, params?: SqlParams, options: Cancelable = {}): Promise<ExecResult> {
    return execute(this.line, { db: this.line.db, sql }, params, options);
  }

  query<T = Record<string, unknown>>(sql: string, params?: SqlParams, options: Cancelable = {}): Promise<T[]> {
    return all<T>(this.line, { db: this.line.db, sql }, params, options);
  }

  iterate<T = Record<string, unknown>>(sql: string, params?: SqlParams, options: Cancelable = {}): AsyncGenerator<T> {
    return rowsOf<T>(this.line, false, { db: this.line.db, sql }, params, options);
  }

  /** A statement to run inside this transaction. */
  async prepare(sql: string, options: Cancelable = {}): Promise<SqliteStatement> {
    const { statement } = await call<{ statement: number }>('sqlite.prepare', { db: this.line.db, sql }, options);
    return new SqliteStatement(this.line, statement, false);
  }
}

/** An open database. */
export class SqliteDatabase {
  readonly id: number;
  private readonly line: Line;

  /** @internal */
  constructor(id: number) {
    this.id = id;
    this.line = new Line(id);
  }

  /** Runs statements that change things; with no parameters the text may hold several statements. */
  exec(sql: string, params?: SqlParams, options: Cancelable = {}): Promise<ExecResult> {
    return this.line.inTurn(() => execute(this.line, { db: this.id, sql }, params, options));
  }

  /** Runs a query and gives all its rows as objects (up to 100000; more is for `iterate`). */
  query<T = Record<string, unknown>>(sql: string, params?: SqlParams, options: Cancelable = {}): Promise<T[]> {
    return this.line.inTurn(() => all<T>(this.line, { db: this.id, sql }, params, options));
  }

  /**
   * Runs a query and gives its rows one by one, in the memory of a few hundred of them. The connection
   * answers nothing else until the loop is over (`BUSY`): leave it or finish it first.
   */
  iterate<T = Record<string, unknown>>(sql: string, params?: SqlParams, options: Cancelable = {}): AsyncGenerator<T> {
    return rowsOf<T>(this.line, true, { db: this.id, sql }, params, options);
  }

  /** Parses a statement once; the statement must be finalized (it is, with the document). */
  async prepare(sql: string, options: Cancelable = {}): Promise<SqliteStatement> {
    const { statement } = await call<{ statement: number }>('sqlite.prepare', { db: this.id, sql }, options);
    return new SqliteStatement(this.line, statement, true);
  }

  /**
   * Runs `work` between BEGIN and COMMIT, and ROLLBACK when it throws. What else asks of the database
   * meanwhile waits for its turn: inside `work` use `tx`, not the database.
   */
  transaction<R>(work: (tx: SqliteTransaction) => Promise<R>, options: Cancelable = {}): Promise<R> {
    return this.line.inTurn(async () => {
      await execute(this.line, { db: this.id, sql: 'BEGIN' }, undefined, options);
      try {
        const result = await work(new SqliteTransaction(this.line));
        await execute(this.line, { db: this.id, sql: 'COMMIT' }, undefined, options);
        return result;
      } catch (error) {
        // The failure of the work is the news; a rollback that fails too only follows it.
        try {
          await execute(this.line, { db: this.id, sql: 'ROLLBACK' }, undefined, {});
        } catch {
          // nothing to add
        }
        throw error;
      }
    });
  }

  close(options: Cancelable = {}): Promise<void> {
    return call<void>('sqlite.close', { db: this.id }, options);
  }
}

if (typeof dispose === 'symbol') {
  Object.defineProperty(SqliteDatabase.prototype, dispose, {
    value(this: SqliteDatabase): Promise<void> {
      return this.close();
    },
  });
}

export const sqlite = {
  /** Opens the database at `path`; the path is held against the scopes of `fs` like any file. */
  open: async (path: string, options: SqliteOpenOptions = {}): Promise<SqliteDatabase> => {
    const { signal, ...flags } = options;
    const { db } = await call<{ db: number }>('sqlite.open', { path, ...flags }, { signal });
    return new SqliteDatabase(db);
  },
};
