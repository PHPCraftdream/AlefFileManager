// SPDX-License-Identifier: MIT OR Apache-2.0
import { McpServer } from './server.ts';
import { connect } from './client.ts';
import type { Identity } from './types.ts';
export const mcp = { server: (info: Identity): McpServer => new McpServer(info), connect };
export { McpServer, type ListenOptions, type ToolOptions, type ResourceOptions, type PromptOptions } from './server.ts';
export { McpClient, connect, type ConnectOptions } from './client.ts';
export { Peer, RpcError, errors } from './core.ts';
export { validate, type Schema } from './schema.ts';
export { revisions, negotiate, type Revision, type Transport, type Context, type Identity, type RequestOptions } from './types.ts';
export type { HttpOptions } from './transports/http-server.ts';
export type { Streams } from './transports/stdio.ts';
