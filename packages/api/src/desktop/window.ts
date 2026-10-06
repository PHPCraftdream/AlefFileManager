// SPDX-License-Identifier: MIT OR Apache-2.0
import { on } from '../core/events.ts';
import { call } from '../core/transport.ts';

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
