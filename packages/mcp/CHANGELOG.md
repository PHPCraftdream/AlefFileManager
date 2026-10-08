# Changelog

## 0.1.0

- Initial private dependency-free browser-safe package over Alef API wrappers.
- Supported MCP revisions: **2025-11-25** and **2025-06-18**. Initialize selects
  an offered supported revision, otherwise returns the newest (2025-11-25).
  Both reject batches. The generic core's explicitly selected March compatibility
  mode is tested independently and is never negotiated by MCP.
- JSON-RPC requests, notifications, standard errors, cancellation and progress;
  bounded client requests and initialization with cleanup.
- Fluent tools, resources and prompts, schema subset validation, explicit stream
  stdio, console adapter, command clients and JSON Streamable HTTP sessions.
- Loopback-only HTTP, Host/Origin guards, generated bearer tokens, bounded
  bodies, POST/GET-405/DELETE semantics and protocol/session headers.
- Secure tokens and session identifiers use Alef `crypto.random`, avoiding the
  absent WebCrypto global observed in native Servo documents.
- Public stdio options use `input`/`output` (legacy `readable`/`writable` aliases
  remain); HTTP uses `allowedOrigins` (`origins` remains an alias).
- Unit tests with Node's built-in type stripping and native HTTP e2e including
  document replacement without runtime exit. No backend changes.
- Limitations: no SSE, subscriptions, resource templates, remote binding/OAuth,
  full JSON Schema or intermediate HTTP progress. Console `stdio: true`
  uses `app.stdin`/`app.stdout`; `server.closed` settles with the input. Native HTTP
  guards remain active even when package guards are explicitly disabled.
