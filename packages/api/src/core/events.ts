// SPDX-License-Identifier: MIT OR Apache-2.0
// One `runtime.events.subscribe` stream per document carries every runtime event as a json frame
// `{ name, payload }`; handlers are matched by name here.
import { AlefError } from './errors.ts';
import { openReadable, type Readable } from './stream.ts';
import { call } from './transport.ts';

type Handler = (payload: unknown) => void;

const handlers = new Map<string, Set<Handler>>();
let subscription: Promise<void> | undefined;

const RESUBSCRIBE_DELAY_MS = 1000;

function dispatch(event: unknown): void {
  if (typeof event !== 'object' || event === null) return;
  const { name, payload } = event as { name?: unknown; payload?: unknown };
  if (typeof name !== 'string') return;
  for (const handler of [...(handlers.get(name) ?? [])]) {
    try {
      handler(payload);
    } catch (error) {
      console.error(`Alef event handler for "${name}" failed:`, error);
    }
  }
}

async function drain(readable: Readable): Promise<void> {
  let slow = false;
  try {
    for await (const frame of readable) {
      if (frame.kind === 'json') dispatch(frame.value);
    }
  } catch (error) {
    slow = error instanceof AlefError && error.code === 'BUSY';
    console.error('Alef event stream failed:', error);
  }
  subscription = undefined;
  // The runtime drops a subscriber that does not keep up (BUSY); the page recovers by subscribing again.
  if (slow && handlers.size > 0) {
    setTimeout(() => {
      if (handlers.size > 0) void open().catch((error: unknown) => console.error('Alef event resubscribe failed:', error));
    }, RESUBSCRIBE_DELAY_MS);
  }
}

function open(): Promise<void> {
  subscription ??= (async () => {
    const { stream } = await call<{ stream: number }>('runtime.events.subscribe', {});
    void drain(await openReadable(stream));
  })().catch((error: unknown) => {
    subscription = undefined;
    throw error;
  });
  return subscription;
}

/**
 * Subscribes `callback` to the runtime event `name`. Resolves, with the function that unsubscribes,
 * once the subscription is in place: events raised after that are not missed. Aborting `signal`
 * unsubscribes as well.
 */
export async function on<T = unknown>(
  name: string,
  callback: (payload: T) => void,
  options: { signal?: AbortSignal } = {},
): Promise<() => void> {
  if (!name) throw new AlefError('INVALID_ARGUMENT', 'Event name is empty.');
  options.signal?.throwIfAborted();
  const handler = callback as Handler;
  const set = handlers.get(name) ?? new Set<Handler>();
  set.add(handler);
  handlers.set(name, set);
  const off = (): void => {
    set.delete(handler);
    if (set.size === 0 && handlers.get(name) === set) handlers.delete(name);
  };
  try {
    await open();
  } catch (error) {
    off();
    throw error;
  }
  options.signal?.addEventListener('abort', off, { once: true });
  if (options.signal?.aborted) off();
  return off;
}
