// SPDX-License-Identifier: MIT OR Apache-2.0
import type { AppInfo, ParsedArgs } from '../../types/index.ts';
import { AlefError } from '../core/errors.ts';
import { on } from '../core/events.ts';
import { call } from '../core/transport.ts';
import { bytesOf, sinkOf } from '../core/web-stream.ts';
import type { Unlisten } from './window.ts';

export interface Cancelable {
  signal?: AbortSignal;
}

/** What the first instance hears when another one starts: its command line and working directory. */
export interface SecondInstance {
  args: ParsedArgs;
  cwd: string;
}

/** The question the application asks before it quits; `preventDefault()` keeps it running. */
export interface QuitRequest {
  readonly defaultPrevented: boolean;
  preventDefault(): void;
}

export interface AppEvents {
  'second-instance': (info: SecondInstance) => void;
  'before-quit': (event: QuitRequest) => void | Promise<void>;
}

type QuitHandler = AppEvents['before-quit'];

// A document answers the quit questions it asked for; one subscription serves all its handlers.
const quitHandlers = new Set<QuitHandler>();
let quitSubscription: Promise<() => void> | undefined;

async function answerQuit(payload: { id: number }): Promise<void> {
  let prevented = false;
  const event: QuitRequest = {
    get defaultPrevented() {
      return prevented;
    },
    preventDefault() {
      prevented = true;
    },
  };
  for (const handler of [...quitHandlers]) {
    try {
      await handler(event);
    } catch (error) {
      console.error('Alef before-quit handler failed:', error);
    }
  }
  await call<void>('app.quitAnswer', { id: payload.id, prevent: prevented });
}

async function listenForQuit(handler: QuitHandler, options: Cancelable): Promise<Unlisten> {
  quitHandlers.add(handler);
  try {
    quitSubscription ??= (async () => {
      const unsubscribe = await on<{ id: number }>(
        'app.before-quit',
        payload => void answerQuit(payload).catch((error: unknown) => console.error('Alef quit answer failed:', error)),
      );
      try {
        await call<void>('app.quitIntercept', { enabled: true });
      } catch (error) {
        unsubscribe();
        throw error;
      }
      return unsubscribe;
    })();
    await quitSubscription;
  } catch (error) {
    quitHandlers.delete(handler);
    if (quitHandlers.size === 0) quitSubscription = undefined;
    throw error;
  }
  let active = true;
  const stop = (): void => {
    if (!active) return;
    active = false;
    quitHandlers.delete(handler);
    if (quitHandlers.size > 0) return;
    const subscription = quitSubscription;
    quitSubscription = undefined;
    void subscription
      ?.then(unsubscribe => {
        unsubscribe();
        return call<void>('app.quitIntercept', { enabled: false });
      })
      .catch((error: unknown) => console.error('Alef quit interception could not be lifted:', error));
  };
  options.signal?.addEventListener('abort', stop, { once: true });
  return stop;
}

/** A stream of the runtime that is asked for when the first piece is wanted, as the web platform has a stream of a file. */
function lazyReadable(open: () => Promise<number>): ReadableStream<Uint8Array> {
  let inner: ReadableStreamDefaultReader<Uint8Array> | undefined;
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      inner ??= bytesOf(await open()).getReader();
      const { done, value } = await inner.read();
      if (done) controller.close();
      else controller.enqueue(value);
    },
    async cancel(reason) {
      await inner?.cancel(reason);
    },
  });
}

/** A sink to a stream of the runtime that is asked for when the first piece is written. */
function lazyWritable(open: () => Promise<number>): WritableStream<Uint8Array> {
  let inner: WritableStreamDefaultWriter<Uint8Array> | undefined;
  return new WritableStream<Uint8Array>({
    async write(chunk) {
      inner ??= sinkOf(await open()).getWriter();
      await inner.write(chunk);
    },
    async close() {
      await inner?.close();
    },
    async abort(reason) {
      await inner?.abort(reason);
    },
  });
}

const opened = async (command: string): Promise<number> => (await call<{ stream: number }>(command, null)).stream;
let standardInput: ReadableStream<Uint8Array> | undefined;
let standardOutput: WritableStream<Uint8Array> | undefined;
let standardError: WritableStream<Uint8Array> | undefined;

/** The running application: identity, command line, environment, lifetime. */
export const app = {
  /** `id`, `name` and `version` come from the manifest, `runtimeVersion` from the Alef runtime. */
  info: (options: Cancelable = {}): Promise<AppInfo> => call<AppInfo>('app.info', null, options),

  /**
   * Asks the runtime to close the window and exit with `code` (0..255, default 0). A `before-quit`
   * handler may veto: the call then resolves and the application keeps running.
   */
  quit: (code?: number, options: Cancelable = {}): Promise<void> =>
    call<void>('app.quit', code === undefined ? {} : { code }, options),

  /**
   * Quits with `code` and never comes back: the promise stays pending, for the process is ending (a
   * `before-quit` handler that vetoes keeps the application running, and the promise pending).
   */
  exit: async (code?: number, options: Cancelable = {}): Promise<never> => {
    await call<void>('app.quit', code === undefined ? {} : { code }, options);
    return new Promise<never>(() => undefined);
  },

  /**
   * What arrives on the standard input, for a console utility (`console: true` in the manifest): the stream
   * ends with the input, and is read once. Any other application gets `NOT_AVAILABLE` when it reads.
   */
  get stdin(): ReadableStream<Uint8Array> {
    standardInput ??= lazyReadable(() => opened('app.stdin'));
    return standardInput;
  },

  /** The standard output of a console utility: what is written leaves whole and in order; `NOT_AVAILABLE` for any other application. */
  get stdout(): WritableStream<Uint8Array> {
    standardOutput ??= lazyWritable(() => opened('app.stdout'));
    return standardOutput;
  },

  /** The standard error of a console utility, as `stdout`. */
  get stderr(): WritableStream<Uint8Array> {
    standardError ??= lazyWritable(() => opened('app.stderr'));
    return standardError;
  },

  /** Starts a new instance with the same arguments and quits this one (also vetoed by `before-quit`). */
  relaunch: (options: Cancelable = {}): Promise<void> => call<void>('app.relaunch', null, options),

  /** The command line, parsed by the `arguments` schema of the manifest. */
  args: (options: Cancelable = {}): Promise<ParsedArgs> => call<ParsedArgs>('app.args', null, options),

  /**
   * One environment variable (`undefined` when it is not set), or every variable of
   * `permissions.app.env` that is set. A name the manifest does not list is `PERMISSION_DENIED`.
   */
  env: (async (name?: string, options: Cancelable = {}): Promise<string | Record<string, string> | undefined> => {
    if (name === undefined) return await call<Record<string, string>>('app.envAll', null, options);
    return (await call<string | null>('app.env', { name }, options)) ?? undefined;
  }) as {
    (name: string, options?: Cancelable): Promise<string | undefined>;
    (name?: undefined, options?: Cancelable): Promise<Record<string, string>>;
  },

  /** The working directory of the process. */
  cwd: (options: Cancelable = {}): Promise<string> => call<string>('app.cwd', null, options),

  /**
   * `true` when this is the first instance of the application of this user; `false` when another
   * one is running: it has been handed this command line and working directory (`second-instance`)
   * and this instance usually quits. Asking again gives the same answer.
   */
  requestSingleInstance: (options: Cancelable = {}): Promise<boolean> =>
    call<boolean>('app.requestSingleInstance', null, options),

  /**
   * `second-instance`: another instance started (only after `requestSingleInstance()` gave `true`).
   * `before-quit`: the application is about to quit by `quit()` or `relaunch()`; call
   * `event.preventDefault()` to keep it running. The handler has 3 seconds to finish; silence allows
   * the quit. Resolves with the function that stops listening.
   */
  on: async <K extends keyof AppEvents>(event: K, handler: AppEvents[K], options: Cancelable = {}): Promise<Unlisten> => {
    if (event === 'second-instance') {
      return await on<SecondInstance>('app.second-instance', handler as AppEvents['second-instance'], options);
    }
    if (event === 'before-quit') return await listenForQuit(handler as QuitHandler, options);
    throw new AlefError('INVALID_ARGUMENT', `Unknown application event "${String(event)}".`);
  },
};
