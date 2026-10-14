# @alef-runtime/runtime

Private 0.1.0 ESM meta-package, MIT OR Apache-2.0. Node >=22.12.0;
no dependencies, no Rust toolchain, no network access, no build step.
CLI bin: `alef`; checkout: `node packages/runtime/bin/alef.mjs`.
Platform binaries ship as optional dependencies
`@alef-runtime/runtime-{win32-x64,darwin-arm64,darwin-x64,linux-x64}`.

## Resolution

`getRuntimePath()` and the `alef` shim resolve the runtime binary in order:

1. `ALEF_BIN` — if set, it must be an existing regular file (symlinked parents
   are followed); anything else is an immediate error, no fallback.
2. Platform package — `require.resolve('@alef-runtime/runtime-<platform>-<arch>/package.json')`,
   then `bin/<binary>` beside it (see table).
3. Otherwise a single error naming the platform, the expected package, the
   cause and the fix (install with `npm install --include=optional`, do not use
   `--omit=optional`, or set `ALEF_BIN`).

| Platform | Package | Binary |
|---|---|---|
| windows-x64 | `@alef-runtime/runtime-win32-x64` | `alef.exe` |
| macos-arm64 | `@alef-runtime/runtime-darwin-arm64` | `alef` |
| macos-x64 | `@alef-runtime/runtime-darwin-x64` | `alef` |
| linux-x64 | `@alef-runtime/runtime-linux-x64` | `alef` |

This table is pinned to the CI build matrix by test (`packages/runtime/test/platforms.test.mjs`).

Platform packages are generated for a release with:

```sh
node packages/runtime/scripts/make-platform-package.mjs <suffix> <binary> <out-dir> <version>
```

## Shim behavior (`bin/alef.mjs`)

On resolution failure the shim prints `alef: <message>` to stderr and exits 1
without spawning anything. On success it spawns the resolved binary directly
with `child_process.spawn(binary, args, { stdio: 'inherit' })` — never through
a shell — forwarding `process.argv.slice(2)`.

Exit codes: the child's exit code is the shim's exit code. A child killed by
signal exits the shim with 128 + signal number, or by the same signal where
Node re-raises it (`process.kill(process.pid, signal)`); an `error` event
(spawn failure) prints `alef: <message>` to stderr and exits 1.

Signals: while a child runs, the shim forwards `SIGINT`/`SIGTERM` to the child
via `child.kill(signal)` and does not exit itself; ESRCH errors are ignored.
On Windows Node cannot deliver real `SIGINT`/`SIGTERM` to the shim process, so
forwarding is best-effort only; console control handling and inherited stdio
apply to the child.

Non-executable `ALEF_BIN`: a plain data file satisfies resolution (it only
checks a regular file). The failure happens at spawn — the `error` event
reports `ENOENT`/`EACCES` per platform, the shim prints `alef: <message>` and
exits 1. There is no shell fallback and no second resolution attempt.

## Versioning

`src/platforms.mjs` exports the single constant
`PLATFORM_VERSION_RELATION = 'workspace-version-placeholder'`: the version
RANGE in this META package's `optionalDependencies`
(`packages/runtime/package.json`). The release generator substitutes the
published version there and passes the same version to each generated
platform package's `version` field. No published version is invented here.
