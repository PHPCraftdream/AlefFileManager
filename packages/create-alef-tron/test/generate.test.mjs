// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtemp, readFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { generate } from '../src/generate.mjs';
import { importToolsCheck } from './helpers.mjs';

const CLOSED_MANIFEST = [
  "id: com.example.app",
  "name:: App",
  "version:: 0.1.0",
  "windows: [",
  "    {",
  "        label: main",
  "        url: /index.html",
  "        width: 800",
  "        height: 600",
  "    }",
  "]",
  "external: {",
  "    connect: []",
  "    load: {",
  "        scripts: []",
  "        styles: []",
  "        images: []",
  "        fonts: []",
  "        media: []",
  "        frames: []",
  "    }",
  "}",
  "permissions: {",
  "    fs: {",
  "        read: []",
  "        write: []",
  "    }",
  "    cli: {",
  "        exec: []",
  "    }",
  "    net: {",
  "        http: []",
  "        socket: []",
  "    }",
  "    shell: {",
  "        openExternal: []",
  "    }",
  "    clipboard: {",
  "        read: false",
  "    }",
  "    shortcut: {",
  "        global: false",
  "    }",
  "    secrets: false",
  "    tray: false",
  "    window: {",
  "        create: false",
  "    }",
  "    app: {",
  "        env: []",
  "        autostart: false",
  "    }",
  "}",
].join('\n') + '\n';

const commonFiles = ['alef.ktav', 'index.html', 'package.json', 'icon.png', '.gitignore', 'README.md', 'src/main.js', 'src/permissions.js'];

async function inScratch(run) {
  const root = await mkdtemp(path.join(tmpdir(), 'create-alef-gen-'));
  try { await run(root); } finally { await rm(root, { recursive: true, force: true }); }
}

test('generated_file_list_matches_bundler_exactly', async () => {
  await inScratch(async root => {
    for (const bundler of ['rsbuild', 'vite']) {
      await generate({ directory: 'app', name: 'App', id: 'com.example.app', bundler }, root);
      const names = (await readdir(path.join(root, 'app'), { recursive: true }))
        .map(name => name.replaceAll(path.sep, '/')).sort();
      if (bundler === 'rsbuild') {
        assert.ok(names.includes('rsbuild.config.mjs'), 'rsbuild output must contain rsbuild.config.mjs');
        assert.ok(!names.includes('vite.config.mjs'), 'rsbuild output must not contain vite.config.mjs');
      } else {
        assert.ok(names.includes('vite.config.mjs'), 'vite output must contain vite.config.mjs');
        assert.ok(!names.includes('rsbuild.config.mjs'), 'vite output must not contain rsbuild.config.mjs');
      }
      for (const file of commonFiles) assert.ok(names.includes(file), `missing ${file} for ${bundler}`);
      await rm(path.join(root, 'app'), { recursive: true, force: true });
    }
  });
});

test('generated_manifest_passes_tools_check_and_is_fully_closed', async () => {
  const check = await importToolsCheck();
  await inScratch(async root => {
    for (const bundler of ['rsbuild', 'vite']) {
      await generate({ directory: 'app', name: 'App', id: 'com.example.app', bundler }, root);
      await check(path.join(root, 'app'));
      const ktav = await readFile(path.join(root, 'app', 'alef.ktav'), 'utf8');
      for (const field of ['id:', 'name::', 'version::', 'windows:', 'external:', 'permissions:']) {
        assert.ok(ktav.includes(field), `manifest must contain ${field}`);
      }
      assert.match(ktav, /id: com\.example\.app/);
      assert.match(ktav, /name:: App/);
      // The whole manifest is pinned: any added grant (a list entry, a true flag, a new
      // permission field) must be a deliberate edit of this expected text.
      assert.equal(ktav, CLOSED_MANIFEST);
      await rm(path.join(root, 'app'), { recursive: true, force: true });
    }
  });
});

test('generated_package_json_is_valid_and_correctly_shaped', async () => {
  await inScratch(async root => {
    for (const bundler of ['rsbuild', 'vite']) {
      await generate({ directory: 'My.App', name: 'My App', id: 'com.example.my-app', bundler }, root);
      const pkg = JSON.parse(await readFile(path.join(root, 'My.App', 'package.json'), 'utf8'));
      assert.equal(pkg.name, 'my.app');
      assert.equal(pkg.scripts.dev, bundler === 'rsbuild' ? 'rsbuild dev' : 'vite');
      assert.equal(pkg.scripts.build, bundler === 'rsbuild' ? 'rsbuild build' : 'vite build');
      assert.deepEqual(pkg['alef-tron'], { dev: 'npm run dev', devUrl: 'http://127.0.0.1:3000', build: 'npm run build', outDir: 'dist' });
      assert.equal(pkg.dependencies['@alef-tron/api'], '^0.1.0');
      assert.equal(pkg.devDependencies['@alef-tron/tools'], '^0.1.0');
      if (bundler === 'rsbuild') assert.equal(pkg.devDependencies['@rsbuild/core'], '2.2.9');
      else assert.equal(pkg.devDependencies.vite, '^6.0.0');
      await rm(path.join(root, 'My.App'), { recursive: true, force: true });
    }
  });
});
