// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import { createServer, request as nativeRequest } from 'node:http';
import { connect } from 'node:net';
const bounded = (promise, ms) => new Promise((resolve, reject) => {
  const timer = setTimeout(() => reject(new Error('MCP runner operation timed out.')), ms);
  promise.then(value => { clearTimeout(timer); resolve(value); }, error => { clearTimeout(timer); reject(error); });
});
import { prepareSite, startApp, verdictOf } from '../lib.mjs';

const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
function listening(port) {
  return new Promise((resolve, reject) => {
    const socket = connect({ host: '127.0.0.1', port });
    const timer = setTimeout(() => { socket.destroy(); reject(new Error('Port probe timed out.')); }, 2000);
    const finish = value => { clearTimeout(timer); socket.destroy(); resolve(value); };
    socket.once('connect', () => finish(true));
    socket.once('error', error => error.code === 'ECONNREFUSED' ? finish(false) : (clearTimeout(timer), reject(error)));
  });
}
// A process without a window needs OpenGL for its hidden WebView; a machine with no driver (a runner of Windows) has none.
const withoutOpenGl = lines => process.platform === 'win32' && lines.some(line => line.includes('no software rendering context'));

/** An MCP server on the console of a utility: JSON lines on stdin and stdout, and the exit with the input. */
async function stdioScenario({ exe, verbose, timeoutMs }) {
  const site = prepareSite('mcp-stdio', 'modules/net/mcp-stdio');
  const running = startApp({ exe, args: ['--app', site], interactive: true, verbose });
  const problems = [];
  try {
    try {
      await running.waitFor(line => line.includes('MCP_STDIO_READY') || line.includes('RESULT FAIL'), timeoutMs, 'the server on the console');
    } catch (error) {
      if (withoutOpenGl(running.lines)) {
        console.log('    skipped: this machine has no OpenGL driver for a process without a window');
        return { problems: [], lines: running.lines };
      }
      throw error;
    }
    const answers = () => running.stdout().toString('utf8').split('\n').filter(Boolean).map(line => JSON.parse(line));
    const answer = async id => {
      const deadline = Date.now() + 15000;
      for (;;) {
        const found = answers().find(message => message.id === id);
        if (found) return found;
        if (Date.now() > deadline) throw new Error(`no answer to request ${id} on stdout`);
        await pause(25);
      }
    };
    const send = message => running.write(`${JSON.stringify(message)}\n`);
    send({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'runner', version: '1' } } });
    const init = await answer(1);
    assert.equal(init.result.protocolVersion, '2025-11-25');
    assert.equal(init.result.serverInfo.name, 'console-mcp');
    send({ jsonrpc: '2.0', method: 'notifications/initialized' });
    send({ jsonrpc: '2.0', id: 2, method: 'tools/list' });
    assert.ok((await answer(2)).result.tools.some(tool => tool.name === 'echo'));
    send({ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'echo', arguments: { text: 'over the console' } } });
    assert.equal((await answer(3)).result.content[0].text, 'over the console');
    send({ jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'echo', arguments: { text: 7 } } });
    assert.equal((await answer(4)).error.code, -32602);
    running.write('{ not json\n');
    await pause(200);
    assert.ok(answers().some(message => message.error?.code === -32700), 'a bad line gets a parse error on stdout');
    console.log('    MCP over the console: initialize, tools/list, tools/call and the refusals passed');
    running.endInput();
    const exit = await running.waitForExit(30000);
    assert.equal(exit?.code, 0, 'the utility leaves when the client closes the input');
    const verdict = verdictOf(running.lines);
    assert.equal(verdict?.[1], 'PASS', verdict?.join(' '));
    assert.ok(answers().every(message => message.jsonrpc === '2.0'), 'nothing but protocol goes to stdout');
  } catch (error) { problems.push(error.message); }
  finally { running.stop(); }
  return { problems, lines: running.lines, site };
}

export function mcpScenarios({ exe, verbose, timeoutMs }) {
  return { 'mcp-stdio': () => stdioScenario({ exe, verbose, timeoutMs }), async mcp() {
    const reserve = createServer();
    await bounded(new Promise((resolve, reject) => { reserve.once('error', reject); reserve.listen(0, '127.0.0.1', resolve); }), 5000);
    const port = reserve.address().port;
    await bounded(new Promise(resolve => reserve.close(resolve)), 5000);
    const site = prepareSite('mcp', 'modules/net/mcp', { replacements: { PORT: String(port) }, targets: { port } });
    const running = startApp({ exe, args: ['--app', site], verbose });
    const problems = [];
    let checks = 0;
    const check = async (name, body) => { await body(); checks++; console.log(`    ok ${name}`); };
    try {
      const line = await running.waitFor(log => log.includes('MCP_READY ') || log.includes('RESULT FAIL'), timeoutMs, 'the MCP URL and token');
      if (line.includes('RESULT FAIL')) throw new Error(line);
      const { url, token } = JSON.parse(line.slice(line.indexOf('MCP_READY ') + 'MCP_READY '.length));
      const headers = { 'content-type': 'application/json', accept: 'application/json, text/event-stream', authorization: `Bearer ${token}` };
      const request = async (message, extra = {}, method = 'POST') => {
        const signal = AbortSignal.timeout(5000);
        const response = await fetch(url, { method, headers: { ...headers, ...extra }, ...(message === undefined ? {} : { body: typeof message === 'string' ? message : JSON.stringify(message) }), signal });
        const text = await response.text();
        return { status: response.status, headers: response.headers, body: text ? (response.headers.get('content-type')?.includes('application/json') ? JSON.parse(text) : text) : undefined };
      };
      const init = version => ({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: version, capabilities: {}, clientInfo: { name: 'runner', version: '1' } } });
      let session;
      await check('The runner initializes the newest revision and receives a session.', async () => {
        const response = await request(init('2025-11-25'));
        assert.equal(response.status, 200); assert.equal(response.body.result.protocolVersion, '2025-11-25');
        session = response.headers.get('mcp-session-id'); assert.ok(session);
      });
      const sessionHeaders = { 'mcp-session-id': session, 'mcp-protocol-version': '2025-11-25' };
      assert.equal((await request({ jsonrpc: '2.0', method: 'notifications/initialized' }, sessionHeaders)).status, 202);
      await check('The runner lists and calls tools through plain fetch.', async () => {
        const list = await request({ jsonrpc: '2.0', id: 2, method: 'tools/list' }, sessionHeaders);
        assert.equal(list.status, 200); assert.ok(list.body.result.tools.some(tool => tool.name === 'echo'));
        const call = await request({ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'echo', arguments: { text: 'runner' } } }, sessionHeaders);
        assert.equal(call.status, 200); assert.equal(call.body.result.content[0].text, 'runner');
      });
      await check('Foreign Origin, wrong Host and missing or bad tokens are refused.', async () => {
        // Node fetch normalizes/overrides Host; a raw HTTP request is needed for this guard probe.
        const wrongHost = await bounded(new Promise((resolve, reject) => {
          const probe = nativeRequest(url, { method: 'POST', headers: { ...headers, host: 'foreign.example' }, agent: false }, response => {
            response.resume(); response.once('end', () => resolve(response.statusCode));
          });
          probe.once('error', reject); probe.setTimeout(5000, () => probe.destroy(new Error('Host probe timed out.')));
          probe.end(JSON.stringify(init('2025-11-25')));
        }), 6000);
        assert.equal(wrongHost, 421);
        for (const [extra, status] of [[{ origin: 'https://foreign.example' }, 403], [{ authorization: '' }, 401], [{ authorization: 'Bearer wrong' }, 401]]) {
          assert.equal((await request(init('2025-11-25'), extra)).status, status);
        }
      });
      await check('The allowedOrigins option reaches the native and package guards.', async () => {
        const response = await request(init('2025-06-18'), { origin: 'https://trusted.example' });
        assert.equal(response.status, 200); assert.equal(response.body.result.protocolVersion, '2025-06-18');
        assert.equal((await request(undefined, { 'mcp-session-id': response.headers.get('mcp-session-id'), 'mcp-protocol-version': '2025-06-18' }, 'DELETE')).status, 204);
      });
      await check('Unsupported offers fall back to newest while both supported revisions reject batches.', async () => {
        for (const version of ['2025-06-18', '2025-03-26']) {
          const response = await request(init(version)); assert.equal(response.status, 200);
          const negotiated = version === '2025-03-26' ? '2025-11-25' : version;
          assert.equal(response.body.result.protocolVersion, negotiated);
          const h = { 'mcp-session-id': response.headers.get('mcp-session-id'), 'mcp-protocol-version': negotiated };
          const batch = await request([{ jsonrpc: '2.0', id: 4, method: 'ping' }], h);
          assert.equal(batch.body.error.code, -32600);
          assert.equal((await request(undefined, h, 'DELETE')).status, 204);
        }
        assert.equal((await request([{ jsonrpc: '2.0', id: 5, method: 'ping' }], sessionHeaders)).body.error.code, -32600);
      });
      await check('Invalid JSON, oversized bodies, unknown sessions and invalid revision headers are refused.', async () => {
        assert.equal((await request('{', sessionHeaders)).status, 400);
        assert.equal((await request('x'.repeat(1024 * 1024 + 1), sessionHeaders)).status, 413);
        assert.equal((await request(init('2025-11-25'), { 'mcp-session-id': 'unknown' })).status, 404);
        assert.equal((await request(init('2025-11-25'), { 'mcp-protocol-version': '2025-03-26' })).status, 400);
        assert.equal((await request(undefined, {}, 'GET')).status, 405);
      });
      await check('Replacing the document closes its MCP port while the runtime stays alive.', async () => {
        assert.equal(await listening(Number(new URL(url).port)), true);
        // The tool requests navigation in the page; the HTTP response may be cut by teardown.
        await request({ jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'replace-document' } }, sessionHeaders).catch(error => { if (!(error instanceof TypeError)) throw error; });
        await running.waitFor(log => log.includes('MCP_DOCUMENT_REPLACED'), 15000, 'the replacement document');
        const deadline = Date.now() + 5000;
        while (await listening(Number(new URL(url).port))) { if (Date.now() >= deadline) throw new Error('The old MCP port still accepts connections.'); await pause(50); }
        assert.equal(running.exit, null, 'The runtime must still be alive.');
        await running.waitFor(log => log.includes('ALEF_E2E RESULT'), 5000, 'the replacement verdict');
        assert.equal(verdictOf(running.lines)?.[1], 'PASS');
      });
      console.log(`    MCP: ${checks} runner checks plus 1 native-client check passed`);
    } catch (error) { problems.push(error.message); }
    finally { running.stop(); await running.waitForExit(5000); }
    return { problems, lines: running.lines };
  } };
}
