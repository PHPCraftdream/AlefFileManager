// SPDX-License-Identifier: MIT OR Apache-2.0
// Public surface of @alef-tron/api. Every function is asynchronous: it returns a Promise or an
// AsyncIterable (checked by packages/api/test/async-surface.test.mjs).
export { AlefError, type AlefErrorCode } from './core/errors.ts';
export { connect, type RuntimeInfo, type RuntimeLimits } from './core/handshake.ts';
export { call, type CallOptions } from './core/transport.ts';
export { openReadable, openWritable, type Readable, type StreamFrame, type Writable } from './core/stream.ts';
export { on } from './core/events.ts';
export {
  AppWindow,
  nativeWindow,
  screen,
  window,
  type CloseRequest,
  type ResizeEdge,
  type Unlisten,
  type WindowEvents,
  type WindowState,
} from './desktop/window.ts';
export { app, type AppEvents, type Cancelable, type QuitRequest, type SecondInstance } from './desktop/app.ts';
export { dialog } from './desktop/dialog.ts';
export { shell } from './desktop/shell.ts';
export { clipboard } from './system/clipboard.ts';
export { notification, type NotificationOptions } from './system/notification.ts';
export { path } from './system/path.ts';
export { os, type OsEvent } from './system/os.ts';
// The DTO `AlefError` of the generated types is the shape of the `AlefError` class above.
export type {
  AppInfo,
  ArgValue,
  ConfirmOptions,
  ErrorCode,
  FileFilter,
  Length,
  MessageKind,
  MessageOptions,
  MonitorInfo,
  OpenOptions,
  OsInfo,
  ParsedArgs,
  Point,
  Rect,
  ResourceId,
  SaveOptions,
  SessionId,
  StreamId,
  Theme,
  WindowDef,
  WindowInfo,
} from '../types/index.ts';
