// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import { existsSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { here, prepareSite, scratch, staticServer } from '../../tests/e2e/lib.mjs';

test('every fixture manifest keeps browser files exclusively in dist', () => {
  let count = 0;
  function walk(directory) {
    const entries = readdirSync(directory, { withFileTypes: true });
    if (existsSync(join(directory, 'alef.ktav'))) {
      count++;
      assert.ok(existsSync(join(directory, 'dist')));
      assert.equal(entries.some(entry => entry.isFile() && /\.(html|js)$/.test(entry.name)), false);
      const manifest = readFileSync(join(directory, 'alef.ktav'), 'utf8');
      const urls = [...manifest.matchAll(/(?:url|entry):\s*(\/[^\s,\]]*)/g)].map(match => match[1]);
      for (const url of urls.length ? urls : ['/']) {
        assert.ok(existsSync(join(directory, 'dist', url === '/' ? 'index.html' : url.slice(1))), `${directory}: ${url}`);
      }
    }
    for (const entry of entries) if (entry.isDirectory() && entry.name !== 'dist') walk(join(directory, entry.name));
  }
  walk(join(here, 'apps'));
  // Floor, not an exact count: new fixtures must not conflict here; the walk must still find them all.
  assert.ok(count >= 36, String(count));
  for (const name of ['data/note.txt', 'secret.txt']) assert.ok(existsSync(join(here, 'apps', 'permissions', name)));
  assert.ok(existsSync(join(here, 'apps', 'startup', 'startup-probe.ps1')));
});

test('prepared sites publish only their own web tree; dev serves dist', { timeout: 240000 }, async () => {
  try {
    for (const app of ['modules/desktop', 'modules/system', 'permissions', 'modules/net/mcp', 'dev']) {
      const site = prepareSite(`layout-${app.replaceAll('/', '-')}`, app, { targets: { app: 'must-not-override-root' } });
      const dist = join(site, 'dist');
      assert.ok(existsSync(join(dist, 'harness.js')));
      assert.ok(existsSync(join(dist, 'src', 'index.js')));
      assert.ok(existsSync(join(dist, 'types')));
      assert.equal(JSON.parse(readFileSync(join(dist, 'targets.json'), 'utf8')).app, site);
      assert.equal(existsSync(join(site, 'harness.js')), false);
      assert.equal(existsSync(join(dist, 'alef.ktav')), false);
      for (const child of ['menu', 'shortcut', 'tray', 'cli', 'cli-commands']) assert.equal(existsSync(join(site, child)), false);
      if (app === 'modules/net/mcp') assert.ok(existsSync(join(dist, 'mcp', 'src', 'index.js')));
      if (app === 'dev') {
        const server = await staticServer(dist);
        try {
          const get = path => fetch(`${server.origin}${path}`, { signal: AbortSignal.timeout(10000) });
          assert.equal((await get('/probe.html')).status, 200);
          assert.equal((await get('/alef.ktav')).status, 404);
        } finally { await server.close(); }
      }
    }
  } finally { rmSync(scratch, { recursive: true, force: true }); }
});
