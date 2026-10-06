// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Length, MonitorInfo, ResizeEdge, WindowDef, WindowInfo } from '../../types/index.ts';
import { AlefError } from '../core/errors.ts';
import { on } from '../core/events.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from './app.ts';

export type { ResizeEdge };

/** The state of a window: sizes and positions in logical pixels (the generated `WindowInfo`). */
export type WindowState = WindowInfo;

export type Unlisten = () => void;

const act = (action: string, fields: Record<string, unknown> = {}, signal?: AbortSignal): Promise<void> =>
  call<void>('window.apply', { action, ...fields }, { signal });

/** The native window of this document. */
export const nativeWindow = {
  getState: (signal?: AbortSignal): Promise<WindowState> => call<WindowState>('window.apply', { action: 'getState' }, { signal }),
  minimize: (signal?: AbortSignal): Promise<void> => act('minimize', {}, signal),
  maximize: (signal?: AbortSignal): Promise<void> => act('maximize', {}, signal),
  restore: (signal?: AbortSignal): Promise<void> => act('restore', {}, signal),
  toggleMaximize: (signal?: AbortSignal): Promise<void> => act('toggleMaximize', {}, signal),
  close: (signal?: AbortSignal): Promise<void> => act('close', {}, signal),
  setDecorations: (enabled: boolean, signal?: AbortSignal): Promise<void> => act('setDecorations', { enabled }, signal),
  setResizable: (enabled: boolean, signal?: AbortSignal): Promise<void> => act('setResizable', { enabled }, signal),
  startDrag: (signal?: AbortSignal): Promise<void> => act('startDrag', {}, signal),
  startResize: (edge: ResizeEdge, signal?: AbortSignal): Promise<void> => act('startResize', { edge }, signal),

  /** Subscribes first, then reads the snapshot; revisions drop stale replies and out-of-order events. */
  async watch(callback: (state: WindowState) => void, signal?: AbortSignal): Promise<Unlisten> {
    let revision = -1;
    let active = true;
    const accept = (state: WindowState): void => {
      if (!active || signal?.aborted || state.revision <= revision) return;
      revision = state.revision;
      callback(state);
    };
    const unsubscribe = await on<WindowState>('runtime.window.state', accept, { signal });
    try {
      accept(await nativeWindow.getState(signal));
    } catch (error) {
      unsubscribe();
      throw error;
    }
    return () => {
      active = false;
      unsubscribe();
    };
  },
};

export interface WindowEvents {
  moved: { label: string; x: number | null; y: number | null };
  resized: { label: string; width: number; height: number; scaleFactor: number };
  focus: { label: string };
  blur: { label: string };
  /** Call `preventDefault()` to keep the window open; the runtime waits for the handlers for 3 seconds. */
  'close-requested': CloseRequest;
}

export interface CloseRequest {
  readonly label: string;
  readonly defaultPrevented: boolean;
  preventDefault(): void;
}

type CloseHandler = (event: CloseRequest) => void | Promise<void>;

// A document answers the close requests of its own window; one subscription serves all its handlers.
const closeHandlers = new Set<CloseHandler>();
let closeSubscription: Promise<() => void> | undefined;

async function answerClose(payload: { label: string; id: number }): Promise<void> {
  let prevented = false;
  const event: CloseRequest = {
    label: payload.label,
    get defaultPrevented() {
      return prevented;
    },
    preventDefault() {
      prevented = true;
    },
  };
  for (const handler of [...closeHandlers]) {
    try {
      await handler(event);
    } catch (error) {
      console.error('Alef close-requested handler failed:', error);
    }
  }
  await call<void>('window.closeAnswer', { id: payload.id, prevent: prevented });
}

async function listenForClose(handler: CloseHandler, options: Cancelable): Promise<Unlisten> {
  closeHandlers.add(handler);
  try {
    closeSubscription ??= (async () => {
      const unsubscribe = await on<{ label: string; id: number }>(
        'window.close-requested',
        payload => void answerClose(payload).catch((error: unknown) => console.error('Alef close answer failed:', error)),
      );
      try {
        await call<void>('window.closeIntercept', { enabled: true });
      } catch (error) {
        unsubscribe();
        throw error;
      }
      return unsubscribe;
    })();
    await closeSubscription;
  } catch (error) {
    closeHandlers.delete(handler);
    if (closeHandlers.size === 0) closeSubscription = undefined;
    throw error;
  }
  let active = true;
  const stop = (): void => {
    if (!active) return;
    active = false;
    closeHandlers.delete(handler);
    if (closeHandlers.size > 0) return;
    const subscription = closeSubscription;
    closeSubscription = undefined;
    void subscription
      ?.then(unsubscribe => {
        unsubscribe();
        return call<void>('window.closeIntercept', { enabled: false });
      })
      .catch((error: unknown) => console.error('Alef close interception could not be lifted:', error));
  };
  options.signal?.addEventListener('abort', stop, { once: true });
  return stop;
}

/** A window of the application, by label. */
export class AppWindow {
  readonly label: string;
  readonly #own: boolean;

  constructor(label: string, own = false) {
    this.label = label;
    this.#own = own;
  }

  #ask<T = void>(op: string, fields: Record<string, unknown> = {}, options: Cancelable = {}): Promise<T> {
    return call<T>(`window.${op}`, { ...fields, label: this.label }, options);
  }

  state = (options: Cancelable = {}): Promise<WindowInfo> => this.#ask<WindowInfo>('state', {}, options);
  setTitle = (title: string, options: Cancelable = {}): Promise<void> => this.#ask('setTitle', { title }, options);
  /** Pixels (logical), or a percentage of the display: `'70%work'`, `'50%screen'`. */
  setSize = (width: Length, height: Length, options: Cancelable = {}): Promise<void> => this.#ask('setSize', { width, height }, options);
  setPosition = (x: Length, y: Length, options: Cancelable = {}): Promise<void> => this.#ask('setPosition', { x, y }, options);
  center = (options: Cancelable = {}): Promise<void> => this.#ask('center', {}, options);
  minimize = (options: Cancelable = {}): Promise<void> => this.#ask('minimize', {}, options);
  maximize = (options: Cancelable = {}): Promise<void> => this.#ask('maximize', {}, options);
  restore = (options: Cancelable = {}): Promise<void> => this.#ask('restore', {}, options);
  toggleMaximize = (options: Cancelable = {}): Promise<void> => this.#ask('toggleMaximize', {}, options);
  setFullscreen = (enabled: boolean, options: Cancelable = {}): Promise<void> => this.#ask('setFullscreen', { enabled }, options);
  setAlwaysOnTop = (enabled: boolean, options: Cancelable = {}): Promise<void> => this.#ask('setAlwaysOnTop', { enabled }, options);
  setResizable = (enabled: boolean, options: Cancelable = {}): Promise<void> => this.#ask('setResizable', { enabled }, options);
  setDecorations = (enabled: boolean, options: Cancelable = {}): Promise<void> => this.#ask('setDecorations', { enabled }, options);
  /** Leaving a side out (or `null`) removes the limit on that axis. */
  setMinSize = (width?: Length | null, height?: Length | null, options: Cancelable = {}): Promise<void> => this.#ask('setMinSize', { width, height }, options);
  setMaxSize = (width?: Length | null, height?: Length | null, options: Cancelable = {}): Promise<void> => this.#ask('setMaxSize', { width, height }, options);
  show = (options: Cancelable = {}): Promise<void> => this.#ask('show', {}, options);
  hide = (options: Cancelable = {}): Promise<void> => this.#ask('hide', {}, options);
  focus = (options: Cancelable = {}): Promise<void> => this.#ask('focus', {}, options);
  /** Closes like the user does: a `close-requested` handler may keep the window open. */
  close = (options: Cancelable = {}): Promise<void> => this.#ask('close', {}, options);
  /** Closes without asking the document. */
  destroy = (options: Cancelable = {}): Promise<void> => this.#ask('destroy', {}, options);
  startDrag = (options: Cancelable = {}): Promise<void> => this.#ask('startDrag', {}, options);
  startResize = (edge: ResizeEdge, options: Cancelable = {}): Promise<void> => this.#ask('startResize', { edge }, options);
  /** Page zoom, 0.25 to 5 (1 is 100 %). */
  setZoom = (factor: number, options: Cancelable = {}): Promise<void> => this.#ask('setZoom', { factor }, options);

  /**
   * Subscribes to an event of this window; resolves with the function that unsubscribes once the
   * subscription is in place. `close-requested` belongs to the document of the window itself.
   */
  async on<E extends keyof WindowEvents>(
    event: E,
    handler: (payload: WindowEvents[E]) => void | Promise<void>,
    options: Cancelable = {},
  ): Promise<Unlisten> {
    if (event === 'close-requested') {
      if (!this.#own) {
        throw new AlefError('INVALID_ARGUMENT', 'Only the document of a window can answer its close requests.');
      }
      return listenForClose(handler as CloseHandler, options);
    }
    return on<WindowEvents[E] & { label: string }>(
      `window.${event}`,
      payload => {
        if (payload.label === this.label) void handler(payload);
      },
      options,
    );
  }
}

/** The windows of the application. */
export const window = {
  /** The window of this document. */
  current: async (options: Cancelable = {}): Promise<AppWindow> => {
    const info = await call<WindowInfo>('window.state', null, options);
    return new AppWindow(info.label, true);
  },

  all: async (options: Cancelable = {}): Promise<AppWindow[]> => {
    const [infos, own] = await Promise.all([
      call<WindowInfo[]>('window.all', null, options),
      call<WindowInfo>('window.state', null, options),
    ]);
    return infos.map(info => new AppWindow(info.label, info.label === own.label));
  },

  /** Opens a window; it needs `permissions.window.create` in the manifest. The window shows itself once its page has something to draw. */
  create: async (definition: WindowDef, options: Cancelable = {}): Promise<AppWindow> => {
    const info = await call<WindowInfo>('window.create', definition, options);
    return new AppWindow(info.label, false);
  },
};

/** The displays and the cursor. */
export const screen = {
  /** Logical pixels; `workArea` leaves out the task bar, Dock and panels. */
  monitors: (options: Cancelable = {}): Promise<MonitorInfo[]> => call<MonitorInfo[]>('screen.monitors', null, options),

  /** The cursor, in the logical pixels of the display it is on; `NOT_AVAILABLE` where the platform does not tell. */
  cursorPosition: (options: Cancelable = {}): Promise<{ x: number; y: number }> =>
    call<{ x: number; y: number }>('screen.cursorPosition', null, options),
};
