# Changelog

## 0.1.0 — M7a.2 (private)

- Dependency-free Node >=22.12.0 ESM resolver `@alef-tron/runtime`: `ALEF_BIN`
  regular file overrides, else platform package via `require.resolve`, else a
  single error naming platform, package, cause and fix. No fallback from an
  invalid `ALEF_BIN` to an installed package.
- Platform table (win32-x64, darwin-arm64, darwin-x64, linux-x64) pinned to the
  CI matrix by test; platform packages carry `os`/`cpu` and one `bin/<binary>`.
- `make-platform-package.mjs` generates one platform package per invocation:
  copied binary (0o755 on non-Windows), generated manifest, both licenses,
  empty-output guard, strict SemVer version argument.
- `bin/alef` shim spawns the resolved binary directly with inherited stdio,
  never through a shell; forwards arguments, the child exit code, 128+signal
  on signal death, and SIGINT/SIGTERM to a running child. Resolution failures
  and spawn `error` events print `alef: <message>` to stderr and exit 1.
  A non-executable `ALEF_BIN` regular file passes resolution and fails at
  spawn (ENOENT/EACCES per platform). Windows signal forwarding is
  best-effort: Node cannot deliver real SIGINT/SIGTERM to the shim.
- `PLATFORM_VERSION_RELATION` is the single workspace-version-placeholder
  constant; the release generator substitutes it. No published version is
  invented; no dependencies; MIT OR Apache-2.0.
