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
export {
  FileHandle,
  fs,
  type FileOpenOptions,
  type ReadStreamOptions,
  type ReadTextOptions,
  type StreamOptions,
  type TreeOptions,
  type WatchOptions,
  type WriteOptions,
} from './data/fs.ts';
export {
  SqliteDatabase,
  SqliteStatement,
  SqliteTransaction,
  sqlite,
  type ExecResult,
  type SqlParams,
  type SqlValue,
  type SqliteOpenOptions,
} from './data/sqlite.ts';
export { secrets } from './data/secrets.ts';
export { Store, store } from './data/store.ts';
export {
  http,
  HttpResponse,
  HttpServer,
  ServerRequest,
  type DownloadOptions,
  type HttpRequestOptions,
  type ServeOptions,
  type ServerResponse,
  type UpgradeOptions,
} from './net/http.ts';
export {
  socket,
  TcpServer,
  TcpSocket,
  UdpSocket,
  type Address,
  type ConnectOptions,
  type Datagram,
  type ListenOptions,
  type TlsOptions,
  type UdpOptions,
} from './net/socket.ts';
export {
  websocket,
  WebSocketConnection,
  WebSocketServer,
  type CloseInfo,
  type WebSocketMessage,
  type WebSocketOptions,
  type WebSocketServeOptions,
} from './net/websocket.ts';
export {
  crypto,
  type Argon2Options,
  type CipherName,
  type Ed25519Keys,
  type HashName,
  type HkdfOptions,
  type ScryptOptions,
  type SealOptions,
} from './data/crypto.ts';
export { dialog } from './desktop/dialog.ts';
export { shell } from './desktop/shell.ts';
export {
  ChildProcess,
  Pty,
  cli,
  type ExecOptions,
  type ExecResult as CliExecResult,
  type KillSignal,
  type PtyOptions,
  type RunOptions,
  type SpawnOptions,
  type WaitResult,
} from './system/cli.ts';
export { clipboard } from './system/clipboard.ts';
export { notification, type NotificationOptions } from './system/notification.ts';
export { path } from './system/path.ts';
export { os, type OsEvent } from './system/os.ts';
// The DTO `AlefError` of the generated types is the shape of the `AlefError` class above.
export type {
  AppInfo,
  ArgValue,
  ConfirmOptions,
  DirEntry,
  FileKind,
  FileStat,
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
  WatchEvent,
  WatchKind,
  WindowDef,
  WindowInfo,
} from '../types/index.ts';
