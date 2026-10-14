# create-alef-runtime

Private 0.1.0 ESM package, MIT OR Apache-2.0. Node >=22.12.0;
no dependencies, build step or runtime binary.
CLI bin: `create-alef-runtime`; checkout: `node packages/create-alef-runtime/bin/create-alef-runtime.mjs`.

## Usage

```sh
create-alef-runtime [directory] [--name <display name>] [--id <reverse.dns.id>]
                 [--bundler rsbuild|vite] [--yes]
```

Defaults: directory `my-alef-app`, display name derived from the directory,
id `com.example.<normalized name>`, bundler `rsbuild`.
`--yes` takes all defaults and never prompts.

The generator refuses an existing non-empty target directory and
invalid ids, directory names or display names; each refusal says why
in the message. It writes files only: no installs and no git.

## Generated project

- `alef.ktav`: version 0.1.0, one window `/index.html`, `icon.png`.
  Every security section is closed: `external.connect` and all
  `external.load` kinds (scripts, styles, images, fonts, media, frames)
  empty; `fs.read`/`fs.write`, `cli.exec`, `net.http`, `net.socket`,
  `shell.openExternal`, `app.env` empty; `clipboard.read`,
  `shortcut.global`, `secrets`, `tray`, `window.create`,
  `app.autostart` false.
- `package.json`: `dev`/`build` scripts, the `alef-runtime` object
  `{ dev, devUrl http://127.0.0.1:3000, build, outDir dist }`,
  devDependencies `@alef-runtime/tools` plus the bundler, dependency
  `@alef-runtime/api`.
- A template page calls `app.info()` and demonstrates handling
  substituted permissions: empty `fs` via `fs.readDir` of
  `path.appData()`, HTTP with its own timeout via `http.request`.
  Timeouts, empty results and `PERMISSION_DENIED` are treated as
  ordinary outcomes, not errors to crash on.

## Versions

| Package | Range | Source |
|---|---|---|
| `@alef-runtime/*` | `^0.1.0` | caret within this repository |
| `@rsbuild/core` | `2.2.9` exact | observed in root `package.json` |
| `vite` | `^6.0.0` | major range, Vite 6 supports Node 22; exact patch not observed here |

Bundlers ship as overlays over the same base template: `rsbuild` adds
`rsbuild.config.mjs`, `vite` adds `vite.config.mjs`; each overlay's
`package.json.template` fills the scripts and devDependencies above.
The `dev`/`build` commands of `@alef-runtime/tools` consume the
`alef-runtime` object.

## Next steps

The CLI prints: `cd <directory>`, `npm install`, `npm run dev`,
`npm run build`, `npx alef-runtime pack`.
