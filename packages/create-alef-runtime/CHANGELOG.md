# Changelog

## 0.1.0 — M7b.4 (private)

- Dependency-free Node >=22.12.0 ESM generator CLI `create-alef-runtime`;
  MIT OR Apache-2.0. Private package, no install or git performed.
- Prompts interactively for directory, display name, id and bundler;
  `--yes` takes all defaults (`my-alef-app`, derived name,
  `com.example.<normalized name>`, `rsbuild`) and never prompts.
- Closed-manifest template: `alef.ktav` 0.1.0 with one window
  `/index.html`, every permission and external section empty/false,
  generated `icon.png` (embedded PNG bytes).
- Both bundler overlays (rsbuild, vite) share the base template and
  emit `package.json` with `dev`/`build` scripts, the `alef-runtime`
  object (`devUrl http://127.0.0.1:3000`, `outDir dist`),
  `@alef-runtime/* ^0.1.0`, `@rsbuild/core` 2.2.9 exact (observed in the
  repository root, where the File Manager app, now alef-file-manager,
  then lived; the pin now lives in `src/generate.mjs`) and `vite ^6.0.0`.
- Validation refusals with reasons: existing non-empty target,
  invalid ids, directory names and display names.
- Template page calls `app.info()` and demonstrates substituted
  permissions (empty `fs` via `fs.readDir`, HTTP with own timeout),
  treating timeouts, empty results and `PERMISSION_DENIED` as
  ordinary outcomes.
- Deferred: bundler builds not executed in this worktree, npm
  publication.
