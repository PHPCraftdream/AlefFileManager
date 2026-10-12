// SPDX-License-Identifier: MIT OR Apache-2.0
import { guard, report, suite, verdict } from './harness.js';
import { mcp } from './mcp/src/index.js';
import { bounded } from './mcp/src/transports/body.js';

guard(async () => {
  if (new URLSearchParams(location.search).has('replaced')) {
    await report('MCP_DOCUMENT_REPLACED');
    await verdict([]);
    return;
  }
  const { check, failed } = suite();
  let navigate;
  const navigation = new Promise(resolve => { navigate = resolve; });
  const server = mcp.server({ name: 'native-mcp', version: '1.0.0' })
    .tool('echo', { inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'], additionalProperties: false } }, args => ({ content: [{ type: 'text', text: args.text }] }))
    .tool('replace-document', { inputSchema: { type: 'object', additionalProperties: false } }, () => { navigate(); return { content: [] }; });
  const targets = await bounded(fetch('./targets.json').then(response => response.json()), 5000);
  await server.listen({ http: { port: targets.port, timeout: 5000, allowedOrigins: ['https://trusted.example'] } });
  await check('mcp-the-native-client-initializes-lists-and-calls-the-server', async () => {
    const client = await mcp.connect({ url: server.url, token: server.token, timeout: 5000 });
    try {
      if (client.peer.revision !== '2025-11-25') throw new Error(`revision ${client.peer.revision}`);
      if (!(await client.listTools()).tools.some(tool => tool.name === 'echo')) throw new Error('No echo tool.');
      if ((await client.callTool('echo', { text: 'native' })).content[0].text !== 'native') throw new Error('Wrong echo result.');
    } finally { await client.close(); }
  });
  if (failed().length) { await verdict(failed()); return; }
  await report(`MCP_READY ${JSON.stringify({ url: server.url, token: server.token })}`);
  await bounded(navigation, 60000);
  await report('MCP_NAVIGATING');
  // Do not close the server: document teardown, not explicit close or runtime exit, must release it.
  location.search = '?replaced=1';
});
