# @alef-tron/tools

Private 0.1.0 ESM package, MIT OR Apache-2.0. Node >=22.12.0;
no dependencies, parser, Rust, runtime binary, generated types or build step.
CLI bin: `alef-tron`; checkout: `node packages/tools/bin/alef-tron.mjs`.

## Commands

```sh
alef-tron keys [name]
alef-tron keys --list
alef-tron pack <dir> [--key <name> | --unsigned] [--out <file>] [--include <names>]
alef-tron verify <file>
alef-tron check [dir]
alef-tron build [dir] [--out <dir>]
alef-tron dev [dir] [--ready-timeout <ms>] [-- <runtime args>]
alef-tron bundle <PACKAGE.alef> --runtime <alef-binary> --platform <win32-x64|darwin-arm64|darwin-x64|linux-x64> [--format <dir|zip|tar.gz|deb|nsis|app>]... [--out <dir>] [--name <name>] [--runtime-version <semver>] [--maintainer <text>] [--allow-unsigned] [--force]
alef-tron --help
```

`keys` creates `default` unless named, refuses overwrite and prints
`<name> <fingerprint>` followed by `key <base64>` (raw32 public Ed25519,
not SPKI or private material). Listing prints names/fingerprints only.
Fingerprints are lowercase SHA-256 of raw32. PKCS8 private PEMs live under
`$ALEF_TRON_HOME/keys`, default `~/.alef-tron/keys`; API `home` overrides the
environment. Creation uses 0700 directories, 0600 files and atomic exclusive
publication; Windows does not provide equivalent POSIX permission guarantees.
Never publish private PEMs.

`pack` signs with `default` or `--key`; missing keys fail, never silently
fall back to unsigned. Explicit `--unsigned` retains hashes but omits signature.
Output defaults to sibling `<folder-basename>.alef`, not a parsed manifest id.
`--out` requires an existing parent. Temporary-file rename publishes output;
existing output inside input is skipped. The CLI prints the output path.

At every depth `.git` and `node_modules` are excluded unless included through
repeated/comma-separated `--include`. Configured key home and case-insensitive
`.alef-tron` basenames are always excluded, even when explicitly included.
Input/output inside key home, symlinked entries, special files and copied
private-key PEM markers fail; a link in the path above the input, the output or
the key home is the user's own layout and is followed. Excluded trees are not traversed. This is not a general secret
scanner: review other secrets before packaging.

`verify` prints `signed by <fingerprint>` or `unsigned`. An embedded key proves
a signature, not developer trust; caller policy must compare fingerprints.
Malformed archives, signatures and hashes fail. Success uses stdout/exit 0;
errors use stderr/exit 1. Unknown, repeated, missing and conflicting options fail.

`check` defaults to cwd: regular nonsymlink root `alef.ktav`, <=1 MiB, nonempty
trimmed strict UTF-8, no BOM or tabs. It explicitly reports that full runtime
validation was not performed. Text hygiene is not Ktav syntax/schema validation;
there are no parser dependencies. `pack`/`verify` check composition, not manifest
semantics. Alef performs full manifest validation before launching the app.

`build` runs the configured build command, then assembles `app-build` (or
`--out`) with `alef.ktav`, optional `icon.png` and the outDir tree under the
manifest assets directory; unsafe or overlapping destinations fail before any
deletion. `dev` starts the configured dev command, polls `alef-tron.devUrl`
until it answers (default 60 s, `--ready-timeout`), then spawns the runtime
with `--app <root> --dev-url <url>` plus any args after `--`; the runtime is
resolved from `ALEF_BIN`, `@alef-tron/runtime` `getRuntimePath()` or `alef`
on PATH. Both children are owned and cleaned up: the dev server is killed when
the runtime exits, on dev death, readiness timeout and SIGINT/SIGTERM; the
runtime exit code propagates. Only `dev` needs the runtime binary (resolved
from `ALEF_BIN`, `@alef-tron/runtime` `getRuntimePath()` or `alef` on PATH);
`build` does not. On Windows, app arguments passed to a `.cmd`/`.bat` runtime
shim are refused when they contain cmd.exe metacharacters (`& | < > ^ % ! "`
or a newline). Not done: full manifest schema validation and bundle.

`bundle` verifies the package first (signature, hashes, format 1; an unsigned
package is refused without `allowUnsigned`; `--no-verify` is deliberately not
implemented), then reads id/name/version/runtime from the verified `alef.ktav`
snapshot. The runtime range is checked ONLY against an explicit
`runtimeVersion`; the runtime binary is never executed (it has no
`--version`); without `runtimeVersion` the check is skipped with a warning.
Range semantics are a strict grammar: only comma-separated comparators with an
operator from `=`, `>`, `>=`, `<`, `<=`, `^`, `~` and a FULL `x.y.z` version
(BigInt u64 cores); prerelease bounds are allowed only for `=` and the
comparisons; wildcards, bare and partial versions are refused with a clear
error; invalid versions/ranges never match. The bundle is named `<name>-<version>-<platform>`; `--name`
overrides the stem (strict ASCII charset, not `.` or `..`; required for
`nsis`; zip entry prefixes are sanitized to the ZIP name charset when the stem
has other characters). Default formats are `dir` plus the platform archive
(zip for win32-x64, tar.gz otherwise). The `dir` layout is `bin/<runtime>`,
`app/app.alef`, LICENSE/NOTICE copied exactly from the runtime's directory
(each capped at 16 MiB, refusal beyond), `bundle.json` with SHA-256 of package
and runtime, and a launcher calling `alef run <package>` (`.cmd` for
win32-x64, `sh` elsewhere; the sh launchers resolve `$0` through symlinks,
the `.cmd` launcher needs no resolution, mode 0755).
Formats: `zip` (unix modes preserved via external attributes, entries limited
to 512 MiB — a bigger runtime must use tar.gz), `tar.gz` (ustar, byte-sorted
entries, deterministic, `SOURCE_DATE_EPOCH` or 0); `deb` (ar + control/data
tar.gz; a `/opt/<pkg>` tree including a `run` wrapper at `/opt/<pkg>/<pkg>`, a
`/usr/bin/<pkg>` symlink to the wrapper, a `.desktop` matching the runtime
writer — Type/Name/Exec/Icon?/Terminal=false/Categories=Utility;/
StartupWMClass, no `Version`; Architecture amd64, Package lowercase kebab,
Maintainer from `--maintainer` or a placeholder); `nsis` (generated
`installer.nsi` with `OutFile`, per-user `$LOCALAPPDATA\Programs`, a Start
Menu shortcut with the `run` argument and a safe `.lnk` name, an HKCU
uninstall key and AppUserModelId; `build-installer.cmd` text; makensis is NOT
run); `app` (a thin `.app` tree, Info.plist with CFBundleIdentifier, launcher
runs `run`; zipped to `<stem>.app.zip` when combined with `zip`, with modes).
Safety: runtime and package paths are walked component-by-component and
symlinked components are refused (OS temp-dir chain exempt); the runtime is at
most 1 GiB: the `dir` and `nsis` layouts copy it streamed with a double hash
check, while the `zip`/`tar.gz` and `app` formats materialize it in memory
(bounded by the 1 GiB runtime limit and the 512 MiB zip entry limit); the
package is bounded and identity-checked, publication is race-safe no-clobber
(`link`/exclusive `mkdir`; `--force` replaces), output targets must be direct
children of the output directory. Refusals: a format unavailable for the platform, existing
outputs without `force`, output lexically at or containing the input paths,
control characters in interpolated values, and `.`/`..` names.

## API

| Export | Input / result |
|---|---|
| `pack(dir, { home?, key?, unsigned?, out?, include? })` | Promise of frozen `{ out, count, status, fingerprint? }` |
| `verify(input)` | Path/URL or Uint8Array; Promise of frozen `{ status, key?, fingerprint?, entries, read }` |
| `createKey(name = 'default', { home? })` | Promise of frozen `{ name, path, key, fingerprint }` |
| `listKeys({ home? })` | Promise of name-sorted public descriptors |
| `keyHome({ home? })` | Absolute key-home path |
| `check(dir = cwd)` | Promise of frozen `{ file, message }` |
| `build(dir, { out?, timeoutMs?, stdio? })` | Promise of frozen `{ dir, out, assets }` |
| `dev(dir, { readyTimeoutMs?, devStdio?, stdio?, appArgs?, alefBin? })` | Promise of frozen `{ code }`; runs until the runtime exits |
| `bundle(packagePath, { runtime?, platform?, formats?, out?, name?, runtimeVersion?, maintainer?, allowUnsigned?, force? })` | Promise of frozen `{ stem, id, name, version, platform, formats, packageSha256, runtimeSha256, out, warnings }` |
| `findRuntime(dir?, { alefBin?, runtimeModule?, path? })` | Promise of the runtime executable path |
| `readZip(input)` | Path/URL or Uint8Array; Promise of frozen `{ entries, read }` |
| `writeZip(entries, { method? })` | Buffer; entries `{ name, data: Uint8Array, method? }`; method 0/8, default automatic |
| `validateName(name)` | Original valid string, otherwise throws; no normalization |

Status is `Signed`/`Unsigned`. Public `key` is a frozen array of 32 bytes;
use `Buffer.from(result.key)`. Frozen entries include metadata and expose
`name`, `size`, `compressedSize`, `method`, `crc`, async `read()`.
Archive/package `read(name)` returns fresh bytes. Disk and memory inputs become
immutable archive snapshots after metadata preflight, with indexed entry names;
opening checks and later reads use that snapshot, never reopen the path.
ZIP reads check CRC. Package verification hashes every file before returning
and rehashes every exposed read, including metadata. Unsigned packages without
hashes use opening digests as integrity baselines, not authenticity.
No retained handle, close API, extraction or runtime launch.

## Strict JS ZIP profile

Names permit only ASCII `A-Z a-z 0-9 . _ @ + / -`, without normalization.
No absolute paths, empty/`.`/`..` segments, trailing dots, Windows device segments
(case-insensitive, with/without extension), segments >255 bytes or names >1024
bytes. Directory entries ending in `/` are forbidden, not ignored.
Case-insensitive duplicate files and file path-prefix conflicts fail; pack
checks these before key/payload reads, without registering traversed directories
as files. Root `alef.ktav` is exact; only `META-INF/hashes` and
`META-INF/signature` are allowed metadata files.

Methods: 0 (stored), 8 (deflate); flags exactly `0x0800`; required version <=20.
No encryption, descriptors, ZIP64, multidisk, local/central extras or
entry/archive comments. EOCD is the final 22 bytes. Local records start at byte
zero, are contiguous and match central order and fields. Prefixes, suffixes,
gaps, overlaps, hidden records and concatenated archives fail. CRC and exact
sizes are checked. Inflation is bounded by declared size +1 and must consume
exactly the compressed input.

Limits: 10,000 entries; 512 MiB uncompressed/file; 2 GiB total uncompressed;
2304 MiB archive; 16 MiB central directory; 16 MiB hashes; 256-byte signature;
10,000 hash lines. Hash text is UTF-8 without BOM or CR: lowercase 64-hex SHA-256,
two spaces, exact asset name, LF per line including the last; no blank lines.
The exact asset set occurs once in ascending ASCII byte order.
Signature text is exactly three LF-terminated lines: `alef-package 1`,
`key <base64>` (raw32), `sig <base64>` (sig64), standard padded canonical base64.
Ed25519 signs `alef-package-v1\n` plus exact hashes bytes. Signature requires
hashes; no signature means Unsigned, still fully checking hashes when present.

The JS verifier uses `node:crypto` for developer convenience. Rust runtime
`verify_strict` is normative; JS success does not guarantee runtime acceptance.
Cross-reader integration remains deferred.

Writer fields: byte-sorted names, DOS 1980-01-01 00:00:00, made-by `0x031E`,
needed version `20` in local and central headers, external attributes
`0x81A40000` (`0o100644` shifted into the high 16 bits), flags `0x0800`,
no extras/comments. Automatic compression
uses level-9 deflate only when smaller, otherwise stored. Explicit ZIP method
selection is a testing API, not a pack option. Determinism requires identical
input bytes, key, options and Node version. Hashes/signatures protect contents
through exact hashes bytes, not ZIP container bytes.

## Rule coverage

Exact assertion-group names, not a claim that the full suite ran here:

| Rule | Exact test coverage |
|---|---|
| ZIP methods and deterministic automatic writer | `zip.test.mjs`: `independent_stored_and_deflated_fixtures_and_writer_output_round_trip_correctly`; `the_writer_fixes_header_fields_sorts_names_and_selects_compression_automatically` |
| ASCII names, directories, limits and reserved conflicts | `zip.test.mjs`: `every_forbidden_ascii_byte_unicode_name_and_malformed_encoding_is_rejected`; `invalid_segments_directories_and_device_names_are_rejected_at_exact_length_boundaries`; `ascii_case_duplicates_and_prefix_conflicts_are_rejected_in_both_orders_and_reserved_names_follow_the_allowlist` |
| Exact flags and forbidden extensions | `zip.test.mjs`: `flags_are_exact_for_both_methods_and_versions_and_methods_are_bounded`; `extras_comments_descriptors_zip64_and_multidisk_archives_are_rejected` |
| Layout and CRC | `zip.test.mjs`: `the_eocd_is_the_final_22_bytes_and_comments_prefixes_suffixes_concatenation_gaps_hidden_locals_and_reordered_central_entries_are_rejected`; `local_and_central_byte_disagreements_and_crc_corruption_identify_the_entry`; `stored_snapshot_reads_check_crc_across_chunk_boundaries_and_return_fresh_bytes` |
| Limits and bounded inflation | `zip.test.mjs`: `declared_limits_precede_payload_allocation_and_inflation`; `inflation_is_bounded_by_declared_size_plus_one_and_consumes_exact_input_and_output` |
| Immutable snapshots | `zip.test.mjs`: `memory_and_disk_snapshots_are_immutable_and_reads_return_fresh_results` |
| Composition and directories | `verify.test.mjs`: `composition_requires_the_exact_root_manifest_and_only_exact_reserved_metadata_paths`; `directory_entries_and_unicode_names_are_rejected_before_package_verification` |
| Canonical hashes and exact set | `verify.test.mjs`: `hashes_reject_every_bom_position_and_malformed_utf8`; `hashes_require_lowercase_hex_two_spaces_and_final_lf_without_any_cr_or_empty_line`; `hashes_require_the_exact_asset_set_once_in_unsigned_ascii_byte_order` |
| Content and crypto failures | `verify.test.mjs`: `signed_and_unsigned_packages_reject_swapped_digests_and_crc_valid_payload_changes`; `crypto_verification_rejects_corrupted_signatures_wrong_keys_prefixes_and_changed_hashes` |
| Canonical signature and base64 | `verify.test.mjs`: `signature_text_requires_exactly_three_canonical_lf_lines_and_valid_utf8_without_bom`; `signature_base64_requires_standard_padding_canonical_pad_bits_and_raw_lengths` |
| Snapshot integrity | `verify.test.mjs`: `opened_packages_keep_original_verified_snapshots_after_crc_colliding_disk_mutations`; `memory_verification_snapshots_inputs_and_returns_fresh_payload_bytes` |
| Focused reader guards | `zip.test.mjs`: `directory_records_are_rejected_by_the_reader`; `prefix_conflicts_are_rejected_in_both_registration_orders`; `the_signature_size_guard_precedes_payload_processing` |
| Focused signature guards | `verify.test.mjs`: `a_corrupted_signature_is_rejected_even_with_valid_asset_hashes`; `the_signature_domain_prefix_is_required` |
| Preflight without opens | `pack.test.mjs`: `invalid_duplicate_and_prefix_names_fail_metadata_preflight_before_any_key_or_payload_opens`; `entry_limits_include_generated_metadata` |

## Validation and deferred work

Root `test:tools` uses default test-runner file concurrency like other suites;
temporary directories isolate files. Metadata mocks replace mass filesystem
creation to reduce disk I/O without increasing existing timeouts. Root lint
already includes packages; `.mjs` is outside current tsc; no tsconfig change.

Observed extra async yields per record motivated synchronous memory metadata
reads and a shared stored-snapshot CRC path for opening checks and later reads.
CRC is checked in 64 KiB chunks, including empty entries, and exposed reads
still return fresh bytes. The central directory remains cached only after its
EOCD bounds/size checks; per-entry and total limits remain before immutable
snapshot allocation and payload processing. Immutable snapshots and the
bounded deflate path are retained.

Deferred: dev/build/create-alef-tron, full JS Ktav/Alef manifest and
environment validation, runtime installation/launch, updater, npm publication
and Rust/JS cross-reader integration. This is not the complete M7 workflow.
