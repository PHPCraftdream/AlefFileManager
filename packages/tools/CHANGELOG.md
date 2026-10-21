# Changelog

## 0.1.0 — M7b.2 (private)

- Runtime of any platform: `alef-runtime fetch-runtime` / `fetchRuntime()` get the
  binary from the npm package `@alef-runtime/runtime-<platform>` (registry read
  directly, sha512 integrity required, https or localhost only) into a cache and
  print its path; `bundle` without `--runtime` fetches the runtime of `--platform`
  after the package was verified; `--cache` and `--registry` choose the cache and
  the registry. `bundle` also copies `LICENSE-*` files next to the runtime into the
  layout. The bundle sources moved to `src/workflow/bundle/` (entry limit).

- Workflow commands: `alef-runtime build [dir] [--out <dir>]` runs the configured
  build command and assembles a packageable app directory (manifest, optional
  icon, assets); `alef-runtime dev [dir] [--ready-timeout <ms>] [-- <args>]` starts
  the configured dev command, polls `alef-runtime.devUrl` for readiness and
  launches the runtime (`ALEF_BIN`, `@alef-runtime/runtime` `getRuntimePath()` or
  PATH `alef`) with `--app`/`--dev-url`, owning and cleaning up both children,
  propagating the runtime exit code and killing the server on runtime exit,
  dev death, readiness timeout or SIGINT/SIGTERM (interrupts kill the runtime's
  own process group, exit 128+n, and the signal exit code is never overwritten
  by a null runtime code). Runtime argv is always argv, never an interpolated
  string; on Windows `.cmd`/`.bat` runtime shims get a fixed ComSpec argv and
  app arguments containing cmd.exe metacharacters are refused. Full manifest
  schema validation and bundle remain deferred.

- Dependency-free Node >=22.12.0 ESM API and `alef-runtime` CLI: keys, pack,
  verify, basic check; MIT OR Apache-2.0. Keys print name/fingerprint and raw32
  public `key <base64>`; listing omits private material; no overwrite.
- Strict JS ZIP profile: ASCII file names, no directory entries; exact flags
  `0x0800`, methods 0/8, required version <=20; no extras, comments, descriptors,
  encryption, ZIP64 or multidisk. Exact contiguous local/central layout and CRC.
  Limits: 10,000 entries, 512 MiB/file, 2 GiB total, 2304 MiB archive,
  16 MiB central directory/hashes, 256-byte signature, 10,000 hash lines.
  Reserved paths, name boundaries and canonical hash/signature fields are
  specified in README with exact test coverage.
- Pack rejects duplicate/prefix file names during metadata preflight before
  key/payload reads; directory traversal does not register directories as files.
  Writer uses sorted names, DOS epoch, 0o100644 and automatic compression:
  level-9 deflate only when smaller. Byte determinism requires identical input,
  key, options and Node version; signatures cover exact hashes, not ZIP bytes.
- Disk/memory inputs become immutable archive snapshots with indexed names;
  package verification hashes every file before returning and on every read.
  `node:crypto` verification is developer convenience; Rust `verify_strict`
  is normative, and JS success does not guarantee runtime acceptance.
- Folder-basename output; key home and `.alef-runtime` always excluded;
  `.git`/`node_modules` explicitly includable. Symlinks, special files and copied
  private PEMs fail. Missing signing keys never silently downgrade to unsigned.
- Basic check: <=1 MiB nonempty UTF-8 without BOM/tabs, not Ktav syntax/schema
  validation. No parser dependencies; Alef fully validates before app launch.
- Observed extra async yields per record motivated synchronous memory metadata
  reads and a shared stored-snapshot CRC path for opening checks and later reads.
  Cached central-directory bounds/size checks and all metadata limits still
  precede immutable snapshot allocation; fresh reads and bounded deflate remain.
  Regression coverage checks stored CRC across 64 KiB boundaries, empty-entry
  CRC failures and fresh bytes; focused reader/signature labels use snake_case.
- Default test-runner file concurrency restored; isolated temporary directories
  and metadata mocks reduce disk I/O without increasing timeouts. Existing lint
  covers packages; no TypeScript configuration change.
- Deferred: bundle/create scaffolding, full JS manifest/environment
  validation, runtime/updater, publication and Rust/JS cross-reader tests.
