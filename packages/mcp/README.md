# @alef-tron/mcp

Private, dependency-free TypeScript MCP package over Alef's existing HTTP and
process wrappers. Import `mcp` from `@alef-tron/mcp`. No Node globals are used by
the implementation; unit tests run with Node's built-in type stripping.

```ts
const server = mcp.server({ name: 'notes', version: '1.0.0' })
  .tool('echo', {
    inputSchema: {
      type: 'object', properties: { text: { type: 'string' } }, required: ['text'],
    },
  }, async args => ({ content: [{ type: 'text', text: args.text }] }));
await server.listen({ http: { port: 0, path: '/mcp' } });
const client = await mcp.connect({ url: server.url!, token: server.token });
await client.callTool('echo', { text: 'Hello.' });
await client.close();
await server.close();
```

## Surface

Servers chain `tool`, `resource` and `prompt`, then `listen` and `close`.
Resource handlers receive `{ uri }`; tool and prompt handlers receive their
arguments. The second handler argument supplies `signal` and
`progress(progress, total?, message?)`. Return MCP result objects (content,
contents or messages) from handlers. Invalid tool inputs produce -32602;
unexpected handler failures produce a sanitized -32603.

Clients connect with `{ command, args }` via `cli.spawn`, `{ url, token }` via
`http.request`, or `{ transport }` for a custom transport. They implement
`listTools`, `callTool`, `listResources`, `readResource`, `listPrompts`,
`getPrompt` and `close`. Request options accept `timeout`, `signal` and
`onProgress`. The default timeout is 30 seconds. Cancellation and progress
notifications work on duplex transports. The JSON-only HTTP transport does
not deliver intermediate progress; SSE is explicitly unsupported, not silently
parsed as JSON. Redirects are not followed by the MCP client.

`listen({ stdio: { input, output } })` uses plain byte streams and newline
JSON (including CRLF), not Content-Length framing. `stdio: true` uses `app.stdin`/`app.stdout` of a console utility, and
`server.closed` settles when the server is closed or the input ends, so that the
utility can leave with its client. The `readable`/`writable` names remain compatibility
aliases. Command clients pipe stdin/stdout, ignore stderr,
and kill/wait for the child during cleanup. EOF rejects outstanding requests;
a nonempty unterminated final line is rejected.

## HTTP security and lifecycle

Only `127.0.0.1`, `::1` and `localhost` bindings are supported. Host and Origin
checks and bearer authentication are enabled by default. A cryptographically
random 256-bit token is exposed as `server.token`; entropy comes from Alef's
`crypto.random` wrapper (including session identifiers), not WebCrypto, which
may be absent in Servo documents. Distribute it through a
trusted channel. `hosts` adds exact authorities (including port); `allowedOrigins` adds
exact serialized origins (`origins` remains a compatibility alias; the requested
`allowedOrigins` takes precedence). Same-loopback HTTP origins are accepted; missing
Origin is allowed for native clients. Explicit `checkHost: false`,
`checkOrigin: false` and `token: false` disable package checks. Native
`http.serve` still enforces its own Host/Origin checks; these options cannot
weaken the runtime guard or permission consent.

POST accepts JSON with both `application/json` and `text/event-stream` in
Accept and responds with JSON or 202 for notifications. GET returns 405 (no SSE
stream). Initialize returns an `Mcp-Session-Id`; subsequent requests must use
that session. DELETE removes it. Invalid/unknown protocol headers are refused;
a missing header on subsequent requests means March for compatibility, and
must match the session revision. Bodies default to 1 MiB, sessions to 128 and
body/dispatch waits to 30 seconds; configure `maxBodyBytes`, `maxSessions` and
`timeout` explicitly. Session state is retained until DELETE or server close.
The HTTP listener uses `http.serve`, so document teardown owns and closes the
native server and its connections exactly as for that wrapper. No separate
runtime/backend lifecycle hooks are introduced.

## JSON Schema subset

Supports boolean schemas, `type` (including unions), `properties`, `required`,
structural `enum`/`const`, single-schema `items`, boolean or schema
`additionalProperties`, `minimum`/`maximum`, Unicode code-point
`minLength`/`maxLength`, JavaScript Unicode `pattern`, `allOf`, `anyOf`, `oneOf`,
`not`, and local `#/$defs/...` references (JSON Pointer escapes included).
Recursion is bounded to 64 levels. Unknown keywords are ignored; this is not a
full JSON Schema validator. Remote refs, tuples, formats, exclusive bounds and
unevaluated properties are not implemented. Schemas/patterns are trusted
application configuration, not untrusted client input.

## Checks

From the repository root: `npm run test:mcp`, `npm run typecheck`,
`npm run lint`, and `npm run lint:structure`. Unit tests use bounded waits and
fake streams/runtime wrappers. Native MCP e2e checks run through the repository
runner with `--only mcp`, including document replacement while the runtime stays alive.

## Protocol revisions

Supported revisions are **2025-11-25** and **2025-06-18**. Both reject JSON-RPC
batches. Unsupported initialize offers (including March) fall back to November.
The generic RPC core retains an explicitly selected March compatibility mode
for standalone core tests only; MCP never negotiates that mode.
