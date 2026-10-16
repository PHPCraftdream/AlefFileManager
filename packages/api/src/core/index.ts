// SPDX-License-Identifier: MIT OR Apache-2.0
// Public surface of the core: errors, handshake, transport, events and streams.
// Every other subpath of @alef-runtime/api builds on these; import them here to
// keep the rest of the API out of the bundle.
export { AlefError, type AlefErrorCode } from './errors.ts';
export { connect, type RuntimeInfo, type RuntimeLimits } from './transport.ts';
export { call, type CallOptions } from './transport.ts';
export { openReadable, openWritable, type Readable, type StreamFrame, type Writable } from './stream.ts';
export { on, type Cancelable, type Unlisten } from './events.ts';
