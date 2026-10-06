// SPDX-License-Identifier: MIT OR Apache-2.0
// Public surface of @alef-tron/api. Every function is asynchronous: it returns a Promise or an
// AsyncIterable (checked by packages/api/test/async-surface.test.mjs).
export { AlefError, type AlefErrorCode } from './core/errors.ts';
export { connect, type RuntimeInfo, type RuntimeLimits } from './core/handshake.ts';
export { call, type CallOptions } from './core/transport.ts';
export { openReadable, openWritable, type Readable, type StreamFrame, type Writable } from './core/stream.ts';
export { on } from './core/events.ts';
export { nativeWindow, type ResizeEdge, type Unlisten, type WindowState } from './desktop/window.ts';
// The DTO `AlefError` of the generated types is the shape of the `AlefError` class above.
export type { ErrorCode, ResourceId, SessionId, StreamId } from '../types/index.ts';
