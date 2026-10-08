// SPDX-License-Identifier: MIT OR Apache-2.0
// Manifests (and command lines) the runtime must refuse before it opens a window: exit code 2 and a
// message that names the cause (docs/stages/m1-core.md: a manifest without `external`/`permissions`
// or with an unknown field is rejected with the path of the error).
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { here } from '../lib.mjs';

const base = readFileSync(join(here, 'apps', 'core', 'alef.ktav'), 'utf8').replaceAll('\r\n', '\n');
const WINDOW = /windows: \[\n    \{\n[\s\S]*?\n    \}\n\]\n/;

function change(text, pattern, replacement) {
  const changed = text.replace(pattern, replacement);
  if (changed === text) throw new Error(`the base manifest no longer contains ${pattern}`);
  return changed;
}

const refuses = (name, manifest, mentions, exitCode = 2, args) => ({ name, manifest, mentions, exitCode, args });

export function manifestCases() {
  const window = base.match(WINDOW)[0];
  const entry = window.slice('windows: [\n'.length, -'\n]\n'.length);
  return [
    refuses('without the external section', change(base, /external: \{\n[\s\S]*?\n\}\n/, ''), ['MANIFEST_INVALID', 'external']),
    refuses('without the permissions section', base.slice(0, base.indexOf('permissions: {')), ['MANIFEST_INVALID', 'permissions']),
    refuses('with an unknown field in a window', change(base, '        height: 600\n', '        height: 600\n        bogus: 1\n'), ['bogus']),
    refuses('with an unknown top-level field', `${base}extra: 1\n`, ['extra']),
    refuses('with an fs scope that is not an absolute path', change(base, '        read: []\n', '        read: [ relative/path ]\n'), ['MANIFEST_INVALID', 'path scope']),
    refuses('with an external.connect entry that is not an origin', change(base, '    connect: []\n', '    connect: [ "not an origin" ]\n'), ['MANIFEST_INVALID', 'connect-src']),
    refuses('with an id that is a path', change(base, 'id: org.alef.e2e.core', 'id: ../evil'), ['MANIFEST_INVALID', 'id']),
    refuses('with a console and a window', `${base}console: true\n`, ['MANIFEST_INVALID', 'console', 'needs windows: []']),
    refuses('with an entry and a window', `${base}entry: /page.js\n`, ['MANIFEST_INVALID', 'entry', 'needs windows: []']),
    refuses('with no window and an entry of another host', `${change(base, WINDOW, 'windows: []\n')}entry: //evil.example/x\n`, ['MANIFEST_INVALID', 'entry']),
    refuses('with two windows of the same label', change(base, WINDOW, `windows: [\n${entry}\n${entry}\n]\n`), ['MANIFEST_INVALID', 'duplicate label']),
    refuses('with a window url that names another host', change(base, 'url: /index.html', 'url: //evil.example/x'), ['MANIFEST_INVALID', 'windows[0].url']),
    refuses('with a minimum window size above the maximum', change(base, '        height: 600\n', '        height: 600\n        minWidth: 500\n        maxWidth: 400\n'), ['MANIFEST_INVALID', 'minwidth']),
    refuses('with an unknown field in the window permissions', change(base, '    app: {', '    window: {\n        create: true\n        bogus: 1\n    }\n    app: {'), ['bogus']),
    refuses('without an alef.ktav', null, ['alef.ktav']),
    refuses('with an unknown command line flag', base, ['unknown argument'], 2, directory => ['--app', directory, '--wat']),
    refuses('with a development URL that is not loopback http', base, ['127.0.0.1'], 2, directory => ['--app', directory, '--dev-url', 'http://example.com/']),
    refuses('asked for help', null, [], 0, () => ['--help']),
  ];
}
