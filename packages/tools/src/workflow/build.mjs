// SPDX-License-Identifier: MIT OR Apache-2.0
import { lstat, realpath, readdir, mkdir, rm, copyFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { readProject } from './project.mjs';
import { runCommand, validateTimeout } from './process.mjs';

async function info(file) {
  try { return await lstat(file); } catch (error) { if (error.code === 'ENOENT') return undefined; throw error; }
}
/** realpath resolves parent links and Windows short names, including absent leaves. */
export async function canonical(file) {
  try { return await realpath(file); } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    const parent = path.dirname(file);
    if (parent === file) throw error;
    const leaf = path.basename(file);
    if (process.platform === 'win32' && (/[ .]$/.test(leaf) || /[:<>"|?*]/.test(leaf) || /^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\.|$)/i.test(leaf))) throw new Error(`Unsafe output/source Windows alias: ${file}`, { cause: error });
    return path.join(await canonical(parent), leaf);
  }
}
const key = file => process.platform === 'win32' ? file.toLowerCase() : file;
function inside(parent, file) {
  const relative = path.relative(key(parent), key(file));
  return relative === '' || (!path.isAbsolute(relative) && relative !== '..' && !relative.startsWith(`..${path.sep}`));
}
async function tree(root, { protect = false } = {}, relative = '') {
  const file = path.join(root, relative); const stat = await info(file);
  if (!stat) return [];
  if (stat.isSymbolicLink()) throw new Error(`Symlink forbidden: ${relative || file}`);
  if (protect && ['package.json', '.git'].includes(path.basename(file).toLowerCase())) throw new Error(`Protected output entry: ${file}`);
  if (stat.isFile()) return [relative];
  if (!stat.isDirectory()) throw new Error(`Unsupported entry: ${relative || file}`);
  const files = [];
  for (const name of await readdir(file)) files.push(...await tree(root, { protect }, path.join(relative, name)));
  return files;
}
async function safeOutput(project, requested) {
  await tree(requested, { protect: true });
  const output = await canonical(requested);
  const source = await canonical(project.outDir);
  if (inside(output, project.root)) throw new Error(`Unsafe output: project or ancestor ${requested}`);
  if (inside(output, source) || inside(source, output)) throw new Error(`Unsafe output: overlaps source ${requested}`);
  for (const name of ['alef.ktav', 'icon.png', 'package.json', '.git']) {
    const control = await canonical(path.join(project.root, name));
    if (inside(output, control) || inside(control, output)) throw new Error(`Unsafe output: overlaps ${name}`);
  }
  return output;
}
/** Build web assets then assemble a packageable application directory. */
export async function build(dir = process.cwd(), options = {}) {
  const project = await readProject(dir);
  const timeoutMs = options.timeoutMs ?? 600000;
  validateTimeout(timeoutMs);
  if (!project.build) throw new Error('package.json: no build command or scripts.build');
  if (options.out !== undefined && (typeof options.out !== 'string' || !options.out.trim())) throw new Error('out must be a nonempty path');
  const requested = path.resolve(project.root, options.out ?? 'app-build');
  await safeOutput(project, requested);
  await runCommand(project.build, { cwd: project.root, timeoutMs, stdio: options.stdio ?? 'inherit' });
  const sourceStat = await info(project.outDir);
  if (!sourceStat) throw new Error(`Missing outDir: ${project.outDir}`);
  if (!sourceStat.isDirectory() || sourceStat.isSymbolicLink()) throw new Error(`outDir must be a non-symlink directory: ${project.outDir}`);
  const files = await tree(project.outDir);
  const icon = path.join(project.root, 'icon.png'); const iconStat = await info(icon);
  if (iconStat && (!iconStat.isFile() || iconStat.isSymbolicLink())) throw new Error(`icon.png must be a regular non-symlink file: ${icon}`);
  // Recheck after author code and immediately before destructive work.
  const output = await safeOutput(project, requested);
  await rm(output, { recursive: true, force: true });
  const assets = path.join(output, project.manifest.assets);
  await mkdir(assets, { recursive: true });
  await writeFile(path.join(output, 'alef.ktav'), project.manifest.bytes);
  if (iconStat) await copyFile(icon, path.join(output, 'icon.png'));
  for (const relative of files) {
    const source = path.join(project.outDir, relative); const target = path.join(assets, relative);
    const stat = await info(source);
    if (!stat?.isFile() || stat.isSymbolicLink()) throw new Error(`Source entry changed: ${relative}`);
    await mkdir(path.dirname(target), { recursive: true }); await copyFile(source, target);
  }
  return Object.freeze({ dir: output, out: output, assets: project.manifest.assets });
}
