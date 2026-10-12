// SPDX-License-Identifier: MIT OR Apache-2.0
import { readFile, realpath, stat } from 'node:fs/promises';
import path from 'node:path';
import { readManifest } from '../check.mjs';

const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
function string(value, field) {
  if (typeof value !== 'string' || !value.trim() || value.includes('\0')) throw new Error(`package.json: ${field} must be a nonempty string`);
  return value;
}
/** Canonical project and validated commands; no commands execute during reading. */
export async function readProject(dir = process.cwd()) {
  const root = await realpath(path.resolve(dir));
  if (!(await stat(root)).isDirectory()) throw new Error(`Project must be a directory: ${root}`);
  const manifest = await readManifest(root);
  let pkg = {};
  try { pkg = JSON.parse(await readFile(path.join(root, 'package.json'), 'utf8')); }
  catch (error) { if (error.code !== 'ENOENT') throw new Error(`package.json: ${error.message}`, { cause: error }); }
  if (!object(pkg)) throw new Error('package.json: must be an object');
  const config = pkg['alef-tron'] ?? {};
  if (!object(config) || (Object.hasOwn(pkg, 'alef-tron') && pkg['alef-tron'] === null)) throw new Error('package.json: alef-tron must be an object');
  if (pkg.scripts !== undefined && !object(pkg.scripts)) throw new Error('package.json: scripts must be an object');
  const commands = {};
  for (const name of ['dev', 'build']) {
    if (Object.hasOwn(config, name)) commands[name] = Object.freeze({ shell: string(config[name], `alef-tron.${name}`) });
    else if (Object.hasOwn(pkg.scripts ?? {}, name)) {
      string(pkg.scripts[name], `scripts.${name}`);
      commands[name] = Object.freeze({ npm: name });
    }
  }
  const outDir = path.resolve(root, Object.hasOwn(config, 'outDir') ? string(config.outDir, 'alef-tron.outDir') : 'dist');
  const devUrl = Object.hasOwn(config, 'devUrl') ? string(config.devUrl, 'alef-tron.devUrl') : undefined;
  if (devUrl !== undefined) {
    let url; try { url = new URL(devUrl); } catch { throw new Error('package.json: alef-tron.devUrl must be an HTTP(S) URL'); }
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) throw new Error('package.json: alef-tron.devUrl must be an HTTP(S) URL');
  }
  return Object.freeze({ root, manifest, outDir, devUrl, ...commands });
}
