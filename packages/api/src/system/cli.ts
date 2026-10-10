// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError } from '../core/errors.ts';
import { call } from '../core/transport.ts';
import { bytesOf, sinkOf } from '../core/web-stream.ts';
import type { Cancelable } from '../desktop/app.ts';

const encoder = new TextEncoder();
const MAX_INPUT = 192 * 1024;

export interface ExecOptions extends Cancelable {
  shell?: boolean | 'powershell';
  cwd?: string;
  env?: Record<string, string>;
  /** Milliseconds the process has; it ends with `TIMEOUT` after that. */
  timeout?: number;
  /**
   * What the process reads on its standard input. It travels in the call body, so it is at most
   * 192 KiB; more goes through `cli.spawn` and streams.
   */
  input?: string | Uint8Array;
}

/** A declared command takes no environment from the page: the manifest decides what runs. */
export type RunOptions = Omit<ExecOptions, 'shell' | 'env'>;

export interface ExecResult {
  code: number | null;
  signal: string | null;
  stdout: string;
  stderr: string;
}

export type KillSignal = 'SIGTERM' | 'SIGKILL' | 'SIGINT';

export interface SpawnOptions extends Cancelable {
  cwd?: string;
  env?: Record<string, string>;
  stdin?: 'pipe' | 'ignore';
  stdout?: 'pipe' | 'ignore';
  stderr?: 'pipe' | 'ignore';
}

export type StartOptions = Omit<SpawnOptions, 'env'>;

export interface WaitResult {
  code: number | null;
  signal: string | null;
}

interface Opened {
  process: number;
  pid: number;
  stdin: number | null;
  stdout: number | null;
  stderr: number | null;
}

/** A process that `cli.spawn` started: its streams carry the bytes with backpressure. */
export class ChildProcess {
  readonly pid: number;
  readonly stdin: WritableStream<Uint8Array> | null;
  readonly stdout: ReadableStream<Uint8Array> | null;
  readonly stderr: ReadableStream<Uint8Array> | null;
  #id: number;

  constructor(opened: Opened) {
    this.#id = opened.process;
    this.pid = opened.pid;
    this.stdin = opened.stdin === null ? null : sinkOf(opened.stdin);
    this.stdout = opened.stdout === null ? null : bytesOf(opened.stdout);
    this.stderr = opened.stderr === null ? null : bytesOf(opened.stderr);
  }

  /** Resolves when the process ends, with its exit code or the signal that stopped it. */
  async wait(): Promise<WaitResult> {
    return call<WaitResult>('cli.wait', { process: this.#id });
  }

  /** Asks the process to stop. */
  async kill(signal?: KillSignal): Promise<void> {
    await call<null>('cli.kill', { process: this.#id, signal });
  }
}

function commandParams(name: string, params?: Record<string, string>, options?: object): void {
  if (typeof name !== 'string' || name === '' || name.includes('\0')) {
    throw new AlefError('INVALID_ARGUMENT', 'a declared command needs a nonempty name without NUL.');
  }
  if (options !== undefined && 'env' in options) {
    throw new AlefError('INVALID_ARGUMENT', 'a declared command takes no environment: the manifest decides what runs.');
  }
  if (params !== undefined && (params === null || typeof params !== 'object' || Array.isArray(params)
    || Object.entries(params).some(([key, value]) => key.includes('\0') || typeof value !== 'string' || value.includes('\0')))) {
    throw new AlefError('INVALID_ARGUMENT', 'command params must map names without NUL to strings without NUL.');
  }
}

export interface PtyOptions extends Cancelable {
  cols: number;
  rows: number;
  cwd?: string;
  env?: Record<string, string>;
}

interface PtyOpened {
  process: number;
  pid: number;
  output: number;
  input: number;
}

function dimensions(cols: number, rows: number): void {
  if (![cols, rows].every(value => Number.isInteger(value) && value >= 1 && value <= 1000)) {
    throw new AlefError('INVALID_ARGUMENT', 'PTY dimensions must be integers from 1 to 1000.');
  }
}

/** A process attached to a terminal, with merged output and byte input. */
export class Pty {
  readonly pid: number;
  readonly readable: ReadableStream<Uint8Array>;
  readonly writable: WritableStream<Uint8Array>;
  #id: number;

  constructor(opened: PtyOpened) {
    this.#id = opened.process;
    this.pid = opened.pid;
    this.readable = bytesOf(opened.output);
    this.writable = sinkOf(opened.input);
  }

  async resize(cols: number, rows: number): Promise<void> {
    dimensions(cols, rows);
    await call<null>('cli.resize', { process: this.#id, cols, rows });
  }

  async kill(signal?: KillSignal): Promise<void> {
    await call<null>('cli.kill', { process: this.#id, signal });
  }

  async wait(): Promise<WaitResult> {
    return call<WaitResult>('cli.wait', { process: this.#id });
  }
}

/**
 * Running other programs. `permissions.cli.exec` lists the programs they may run: `exec` takes a
 * command line (a shell splits it, when `shell` says so), `spawn` a program and its arguments, and
 * gives back a `ChildProcess` whose streams carry the bytes.
 */
export const cli = {
  /** Runs a command declared in `permissions.cli.commands`, without a shell. */
  run: async (name: string, params?: Record<string, string>, options: RunOptions = {}): Promise<ExecResult> => {
    commandParams(name, params, options);
    const { cwd, timeout, input, signal } = options;
    const body = (input === undefined
      ? undefined
      : typeof input === 'string' ? encoder.encode(input) : input) as Uint8Array<ArrayBuffer> | undefined;
    if (body !== undefined && body.length > MAX_INPUT) {
      throw new AlefError('INVALID_ARGUMENT', 'input above 192 KiB does not fit a call: pass it through cli.start streams.');
    }
    return call<ExecResult>('cli.run', { name, params, cwd, timeoutMs: timeout }, { signal, body });
  },

  /** Starts a declared command with the same streams and process resource as `spawn`. */
  start: async (name: string, params?: Record<string, string>, options: StartOptions = {}): Promise<ChildProcess> => {
    commandParams(name, params, options);
    const { cwd, stdin, stdout, stderr, signal } = options;
    return new ChildProcess(await call<Opened>('cli.start', { name, params, cwd, stdin, stdout, stderr }, { signal }));
  },

  exec: async (commandLine: string, options: ExecOptions = {}): Promise<ExecResult> => {
    const { shell, cwd, env, timeout, input, signal } = options;
    if (typeof commandLine !== 'string' || commandLine.trim() === '') throw new AlefError('INVALID_ARGUMENT', 'exec needs a command line.');
    const body = (input === undefined
      ? undefined
      : typeof input === 'string' ? encoder.encode(input) : input) as Uint8Array<ArrayBuffer> | undefined;
    if (body !== undefined && body.length > MAX_INPUT) {
      throw new AlefError('INVALID_ARGUMENT', 'input above 192 KiB does not fit a call: pass it through cli.spawn streams.');
    }
    return call<ExecResult>('cli.exec', { commandLine, shell, cwd, env: env && Object.entries(env), timeoutMs: timeout }, { signal, body });
  },

  pty: async (program: string, args: string[] | undefined, options: PtyOptions): Promise<Pty> => {
    const { cols, rows, cwd, env, signal } = options;
    if (typeof program !== 'string' || program === '') throw new AlefError('INVALID_ARGUMENT', 'pty needs a program.');
    dimensions(cols, rows);
    return new Pty(await call<PtyOpened>('cli.pty', { program, args, cols, rows, cwd, env: env && Object.entries(env) }, { signal }));
  },

  spawn: async (program: string, args?: string[], options: SpawnOptions = {}): Promise<ChildProcess> => {
    const { cwd, env, stdin, stdout, stderr, signal } = options;
    if (typeof program !== 'string' || program === '') throw new AlefError('INVALID_ARGUMENT', 'spawn needs a program.');
    return new ChildProcess(await call<Opened>('cli.spawn', { program, args, cwd, env: env && Object.entries(env), stdin, stdout, stderr }, { signal }));
  },
};
