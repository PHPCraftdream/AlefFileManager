// SPDX-License-Identifier: MIT OR Apache-2.0
// An MCP server on the console of a utility: the runner speaks JSON lines on stdin and reads the answers on stdout;
// the page leaves when the runner closes the input. Nothing but protocol goes to stdout.
import { api, guard, report, suite, verdict } from './harness.js';
import { mcp } from './mcp/src/index.js';

const { app } = api;

guard(async () => {
  const { check, failed } = suite();
  const server = mcp.server({ name: 'console-mcp', version: '1.0.0' })
    .tool('echo', { inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'], additionalProperties: false } }, args => ({ content: [{ type: 'text', text: args.text }] }));
  await check('mcp-stdio-the-server-listens-on-the-console', async () => {
    await server.listen({ stdio: true });
  });
  await report('MCP_STDIO_READY');
  await server.closed;
  await check('mcp-stdio-the-server-is-closed-with-the-input', async () => {
    if (failed().length) throw new Error('an earlier check failed');
  });
  await verdict(failed());
  await app.exit(0);
});
