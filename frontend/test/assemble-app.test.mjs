// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { assembleControls } from '../assemble-app.ts';

test('assembly copies only controls, preserving built web assets', () => {
  const root = mkdtempSync(join(tmpdir(), 'alef-assembly-'));
  try {
    const source = join(root, 'source');
    const app = join(root, 'app');
    mkdirSync(source);
    mkdirSync(join(app, 'dist'), { recursive: true });
    writeFileSync(join(source, 'alef.ktav'), 'id: test');
    writeFileSync(join(source, 'icon.png'), Buffer.from([0, 255, 1]));
    writeFileSync(join(source, 'private.txt'), 'private');
    writeFileSync(join(app, 'dist', 'index.html'), 'web');
    assembleControls(source, app);
    assert.deepEqual(readdirSync(app).sort(), ['alef.ktav', 'dist', 'icon.png']);
    assert.deepEqual(readFileSync(join(app, 'icon.png')), Buffer.from([0, 255, 1]));
    assert.equal(readFileSync(join(app, 'dist', 'index.html'), 'utf8'), 'web');
    assert.equal(existsSync(join(app, 'dist', 'alef.ktav')), false);
    writeFileSync(join(source, 'alef.ktav'), 'id: updated');
    assembleControls(source, app);
    assert.equal(readFileSync(join(app, 'alef.ktav'), 'utf8'), 'id: updated');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
