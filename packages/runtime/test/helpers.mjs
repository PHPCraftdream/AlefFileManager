// SPDX-License-Identifier: MIT OR Apache-2.0
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';

export const root = fileURLToPath(new URL('../', import.meta.url));
export function temporary(t) {
  const directory = mkdtempSync(join(root, '.test-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}
export async function installed(t, entry) {
  const directory = temporary(t);
  const meta = join(directory, 'node_modules', '@alef-runtime', 'runtime');
  mkdirSync(meta, { recursive: true });
  cpSync(join(root, 'src'), join(meta, 'src'), { recursive: true });
  cpSync(join(root, 'package.json'), join(meta, 'package.json'));
  let binary;
  if (entry) {
    const dependency = join(directory, 'node_modules', '@alef-runtime', `runtime-${entry.suffix}`);
    mkdirSync(join(dependency, 'bin'), { recursive: true });
    writeFileSync(join(dependency, 'package.json'), JSON.stringify({ name: `@alef-runtime/runtime-${entry.suffix}` }));
    binary = join(dependency, 'bin', entry.binary);
    writeFileSync(binary, 'fake runtime');
  }
  const resolver = await import(pathToFileURL(join(meta, 'src', 'resolve.mjs')).href);
  const api = await import(pathToFileURL(join(meta, 'src', 'index.mjs')).href);
  return { directory, binary, ...resolver, ...api };
}
export function generate(...args) {
  const result = spawnSync(process.execPath, [join(root, 'scripts', 'make-platform-package.mjs'), ...args], {
    encoding: 'utf8', timeout: 10_000, windowsHide: true,
  });
  if (result.error) throw result.error;
  return result;
}
