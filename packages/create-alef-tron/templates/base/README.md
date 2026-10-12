# {{NAME}}

Generated Alef application (`{{ID}}`). Requires Node >=22.12.0.

```sh
npm install
npm run dev
npm run build
npx alef-tron pack
```

The bundler serves http://127.0.0.1:3000 and builds into `dist`. Run the
page in Alef to use the native API; a plain browser has no native bridge.
The package's `alef-tron` object records the dev/build commands and output.

`alef.ktav` is deliberately closed: no filesystem, network, external resource,
clipboard-read, command, tray, autostart or runtime-window-creation grants.
The button demonstrates ordinary permission denial, not runtime detection.
App data and https://example.com are stand-ins; grant only the narrow scopes
needed by your application. The HTTP example reads status only, never a body.

Display names allow Unicode letters/numbers, spaces and `._-!?()` only.
The manifest uses `name::` so keyword-looking display text remains a string.
The application ships a generated PNG at root `icon.png`; the runtime discovers
that conventional filename. There is no manifest icon field.

The generator runs no install or git commands. Generation uses exclusive file
creation and best-effort owned-file cleanup, not atomic protection against
malicious concurrent directory replacement.
