let capability: string | undefined;

/** In-process Servo transport; no server address or OS endpoint. */
export async function invoke<T>(command: string, arguments_: unknown = null, signal?: AbortSignal): Promise<T> {
  if (capability === undefined) {
    capability = new URLSearchParams(window.location.hash.slice(1)).get('capability') ?? '';
  }
  if (!capability) throw new Error('Start the native application with npm run dev or npm start.');
  const response = await fetch('native://invoke/', {
    method: 'POST',
    headers: { Authorization: `Bearer ${capability}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({ command, arguments: arguments_ }),
    signal,
  });
  const body = await response.json() as T | { error: string };
  if (!response.ok) {
    throw new Error(typeof body === 'object' && body !== null && 'error' in body
      ? body.error : `Native command failed (${response.status})`);
  }
  return body as T;
}

export type Unlisten = () => void;

/** Subscription exists until unlisten, signal abort, or document disposal. */
export function listen<T>(
  name: string,
  callback: (payload: T) => void,
  options: { signal?: AbortSignal } = {},
): Unlisten {
  if (!name) throw new Error('Event name is empty.');
  const handler = (event: Event) => {
    if (!(event instanceof CustomEvent)) return;
    const detail = event.detail as { name?: string; payload: T } | null;
    if (detail?.name === name) callback(detail.payload);
  };
  window.addEventListener('__alef_runtime_event__', handler, options);
  return () => window.removeEventListener('__alef_runtime_event__', handler);
}

export type ResizeEdge = 'north' | 'northEast' | 'east' | 'southEast'
  | 'south' | 'southWest' | 'west' | 'northWest';

export interface WindowState {
  revision: number;
  title: string;
  width: number;
  height: number;
  x: number | null;
  y: number | null;
  scaleFactor: number;
  focused: boolean;
  maximized: boolean;
  minimized: boolean | null;
  visible: boolean | null;
  decorated: boolean;
  resizable: boolean;
  fullscreen: boolean;
  supportsDragResize: boolean;
}

const windowCommand = (action: string, fields: Record<string, unknown> = {}, signal?: AbortSignal) =>
  invoke<void>('runtime.window', { action, ...fields }, signal);

export const nativeWindow = {
  getState: (signal?: AbortSignal) => invoke<WindowState>('runtime.window', { action: 'getState' }, signal),
  minimize: (signal?: AbortSignal) => windowCommand('minimize', {}, signal),
  maximize: (signal?: AbortSignal) => windowCommand('maximize', {}, signal),
  restore: (signal?: AbortSignal) => windowCommand('restore', {}, signal),
  toggleMaximize: (signal?: AbortSignal) => windowCommand('toggleMaximize', {}, signal),
  close: (signal?: AbortSignal) => windowCommand('close', {}, signal),
  setDecorations: (enabled: boolean, signal?: AbortSignal) => windowCommand('setDecorations', { enabled }, signal),
  setResizable: (enabled: boolean, signal?: AbortSignal) => windowCommand('setResizable', { enabled }, signal),
  startDrag: (signal?: AbortSignal) => windowCommand('startDrag', {}, signal),
  startResize: (edge: ResizeEdge, signal?: AbortSignal) => windowCommand('startResize', { edge }, signal),

  /** Subscribe first, then read the snapshot; revisions reject stale RPC responses. */
  async watch(callback: (state: WindowState) => void, signal?: AbortSignal): Promise<Unlisten> {
    let revision = -1;
    let active = true;
    const accept = (state: WindowState) => {
      if (!active || signal?.aborted || state.revision <= revision) return;
      revision = state.revision;
      callback(state);
    };
    const unsubscribe = listen<WindowState>('runtime.window.state', accept, { signal });
    try {
      accept(await nativeWindow.getState(signal));
    } catch (error) {
      unsubscribe();
      throw error;
    }
    return () => { active = false; unsubscribe(); };
  },
};
