// SPDX-License-Identifier: MIT OR Apache-2.0
// Assemble a verified .alef package plus a runtime binary into distributable
// bundles: plain dir layout, zip, tar.gz, deb, NSIS staging dir, macOS .app and dmg.
// Every rejection happens before the first output byte is written.
import { createHash, randomUUID } from 'node:crypto';
import { execFile } from 'node:child_process';
import { constants, readFileSync, realpathSync } from 'node:fs';
import { lstat, link, mkdir, open, readdir, realpath, rename, rm, unlink, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { tmpdir } from 'node:os';
import { promisify } from 'node:util';
import { noSymlinks } from '../keys.mjs';
import { verify } from '../verify.mjs';
import { writeZip } from '../zip.mjs';
import { buildDeb, escapeDesktopValue, quoteDesktopExec, renderBuildInstallerCmd, renderInfoPlist, renderNsisInstaller, writeGzip, writeTar } from './bundle-formats.mjs';
import { parseVersion, readManifestFields, satisfiesRange } from './bundle-manifest.mjs';
const GIB = 1024 * 1024 * 1024;
const MIB = 1024 * 1024;
const MAX_PACKAGE = 2304 * MIB;
const ZIP_ENTRY_LIMIT = 512 * MIB;
const LICENSE_LIMIT = 16 * MIB;
const check = (condition, message) => { if (!condition) throw new Error(`Bundle: ${message}`); };
const within = (parent, child) => {
  const relative = path.relative(parent, child);
  return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
};
const hash = bytes => createHash('sha256').update(bytes).digest('hex');

// Target table: file suffix, node platform/arch pair and runtime binary name.
const PLATFORMS = Object.freeze({
  'win32-x64': Object.freeze({ suffix: '.exe', platform: 'win32', arch: 'x64', binary: 'alef.exe' }),
  'darwin-arm64': Object.freeze({ suffix: '', platform: 'darwin', arch: 'arm64', binary: 'alef' }),
  'darwin-x64': Object.freeze({ suffix: '', platform: 'darwin', arch: 'x64', binary: 'alef' }),
  'linux-x64': Object.freeze({ suffix: '', platform: 'linux', arch: 'x64', binary: 'alef' }),
});
const FORMATS = ['dir', 'zip', 'tar.gz', 'deb', 'nsis', 'app', 'dmg'];
const NOFOLLOW = constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0);
// Ancestor symlinks are tolerated only inside the OS temp-dir chain (mkdtemp roots).
const realTmp = realpathSync(tmpdir());

// Walk every directory component from the filesystem root down to the parent;
// refuse any symlink unless it IS the temp dir or contains it.
async function noSymlinkComponents(absolutePath) {
  const root = path.parse(absolutePath).root;
  const components = [];
  for (let current = path.dirname(absolutePath); current !== root; current = path.dirname(current)) {
    components.unshift(current);
  }
  for (const current of components) {
    if (!(await lstat(current)).isSymbolicLink()) continue;
    const real = await realpath(current);
    if (realTmp === real || realTmp.startsWith(real + path.sep)) continue;
    throw new Error(`Bundle: Symlink forbidden in path: ${current}`);
  }
}

// Tool version for bundle.json; a broken layout must not break bundling.
function toolVersion() {
  try { return String(JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8')).version ?? '0.0.0-dev'); }
  catch { return '0.0.0-dev'; }
}

// Exact, identity-checked full read of the package (bounded like pack.mjs).
async function readPackage(packagePath) {
  await noSymlinkComponents(packagePath);
  const handle = await open(packagePath, NOFOLLOW);
  try {
    const stat = await handle.stat();
    check(stat.isFile(), 'package must be a regular file');
    check(stat.size <= MAX_PACKAGE, `package exceeds the ${MAX_PACKAGE / MIB} MiB limit`);
    const bytes = Buffer.alloc(stat.size); let size = 0;
    while (size < bytes.length) {
      const { bytesRead } = await handle.read(bytes, size, bytes.length - size, size);
      check(bytesRead > 0, 'package file shrank'); size += bytesRead;
    }
    const extra = Buffer.alloc(1);
    check((await handle.read(extra, 0, 1, size)).bytesRead === 0, 'package file grew');
    check((await handle.stat()).size === stat.size, 'package file changed while bundling');
    return bytes;
  } finally { await handle.close(); }
}

// Exact full read of a trusted runtime-side file (LICENSE/NOTICE); oversize is refused.
async function exactRead(file, limit) {
  const handle = await open(file, NOFOLLOW);
  try {
    const stat = await handle.stat();
    check(stat.isFile(), `${path.basename(file)} is not a regular file`);
    check(stat.size <= limit, `${path.basename(file)} exceeds the ${limit / MIB} MiB limit`);
    const bytes = Buffer.alloc(stat.size); let size = 0;
    while (size < bytes.length) {
      const { bytesRead } = await handle.read(bytes, size, bytes.length - size, size);
      check(bytesRead > 0, 'file shrank while reading'); size += bytesRead;
    }
    return bytes;
  } finally { await handle.close(); }
}

// Streaming SHA-256 pass over the runtime without buffering it whole.
async function streamHash(file, size) {
  const digest = createHash('sha256');
  const handle = await open(file, NOFOLLOW);
  try {
    const stat = await handle.stat();
    check(stat.isFile() && stat.size === size, 'runtime binary changed while bundling');
    const buffer = Buffer.alloc(Math.max(1, Math.min(size, MIB))); let done = 0;
    while (done < size) {
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, done);
      check(bytesRead > 0, 'runtime binary changed while bundling');
      done += bytesRead; digest.update(buffer.subarray(0, bytesRead));
    }
    check((await handle.stat()).size === size, 'runtime binary changed while bundling');
  } finally { await handle.close(); }
  return digest.digest('hex');
}

// Stream-copy the runtime to target while hashing; caller compares digests.
async function copyRuntime(target, file, size) {
  const digest = createHash('sha256');
  const source = await open(file, NOFOLLOW);
  try {
    const stat = await source.stat();
    check(stat.isFile() && stat.size === size, 'runtime binary changed while bundling');
    const buffer = Buffer.alloc(Math.max(1, Math.min(size, MIB))); let done = 0;
    const dest = await open(target, 'wx', 0o755);
    try {
      while (done < size) {
        const { bytesRead } = await source.read(buffer, 0, buffer.length, done);
        check(bytesRead > 0, 'runtime binary changed while bundling');
        done += bytesRead; digest.update(buffer.subarray(0, bytesRead));
        await dest.write(buffer.subarray(0, bytesRead));
      }
      await dest.sync();
    } finally { await dest.close(); }
    check(done === size && (await source.stat()).size === size, 'runtime binary changed while bundling');
  } finally { await source.close(); }
  return digest.digest('hex');
}

// Race-safe single-file publication: wx temp + exclusive link; force replaces once.
async function publishFile(target, bytes, force = false) {
  const temp = path.join(path.dirname(target), `.alef-bundle-${randomUUID()}`);
  const handle = await open(temp, 'wx', 0o600);
  try {
    try { await handle.writeFile(bytes); await handle.sync(); } finally { await handle.close(); }
    try {
      await link(temp, target);
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
      if (force !== true) throw new Error(`Bundle: ${target} already exists (use --force)`, { cause: error });
      await rm(target, { recursive: true, force: true });
      await link(temp, target);
    }
  } finally { await unlink(temp).catch(err => { if (err.code !== 'ENOENT') throw err; }); }
}

// Race-safe directory publication: exclusive mkdir, then move staged children in.
async function publishDir(stage, target, force = false) {
  try {
    await mkdir(target, { recursive: false });
  } catch (error) {
    if (error.code !== 'EEXIST') throw error;
    if (force !== true) throw new Error(`Bundle: ${target} already exists (use --force)`, { cause: error });
    await rm(target, { recursive: true, force: true });
    await mkdir(target, { recursive: false });
  }
  for (const child of (await readdir(stage)).sort()) {
    await rename(path.join(stage, child), path.join(target, child));
  }
}

// Stage the in-memory layout files, then stream-copy the runtime into bin/.
async function writeLayout(stage, files, runtimePath, runtimeSize, runtimeSha256, binary) {
  for (const file of files) {
    const target = path.join(stage, ...file.rel.split('/'));
    await mkdir(path.dirname(target), { recursive: true });
    await writeFile(target, file.data, { mode: file.mode });
  }
  await mkdir(path.join(stage, 'bin'), { recursive: true });
  const digest = await copyRuntime(path.join(stage, 'bin', binary), runtimePath, runtimeSize);
  check(digest === runtimeSha256, 'runtime binary changed while bundling');
}

// hdiutil takes an argument vector only (no shell); `options.hdiutil` replaces it in tests.
const runHdiutil = args => promisify(execFile)('hdiutil', args, { timeout: 30 * 60 * 1000, maxBuffer: 4 * MIB });

// The volume name shown when the image is mounted: the application name without path and control characters.
function volumeName(name, fallback) {
  const unsafe = char => char.charCodeAt(0) < 32 || char.charCodeAt(0) === 127 || '/:'.includes(char) || char.charCodeAt(0) === 92;
  const clean = [...name].map(char => (unsafe(char) ? '_' : char)).join('').trim().slice(0, 27).trim();
  return clean === '' ? fallback.slice(0, 27) : clean;
}

// Race-safe publication of a finished file by an exclusive link; force replaces once.
async function publishLinked(target, source, force = false) {
  try {
    await link(source, target);
  } catch (error) {
    if (error.code !== 'EEXIST') throw error;
    if (force !== true) throw new Error(`Bundle: ${target} already exists (use --force)`, { cause: error });
    await rm(target, { recursive: true, force: true });
    await link(source, target);
  }
}

// Debian package names: lowercase, non-alphanumeric runs collapse to '-'.
function kebab(name) {
  const pkg = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  check(pkg !== '' && pkg.length <= 40 && /^[a-z0-9][a-z0-9+.-]*$/.test(pkg),
    `deb: Package: cannot derive a valid Debian package name from '${name}'`);
  return pkg;
}

/**
 * Bundle a signed package with a runtime binary for one target platform.
 * Returns a frozen result; all errors are `Bundle: ...` and precede any write.
 */
export async function bundle(packagePath, options = {}) {
  const warnings = [];
  const entry = PLATFORMS[options.platform];
  check(entry !== undefined, `unknown platform ${String(options.platform)}`);
  const platform = options.platform;
  const formats = [...new Set(options.formats ?? ['dir', platform.startsWith('win32') ? 'zip' : 'tar.gz'])];
  check(Array.isArray(formats) && formats.length > 0, 'formats must be a nonempty array of strings');
  check(options.hdiutil === undefined || typeof options.hdiutil === 'function', 'hdiutil must be a function or undefined');
  for (const format of formats) {
    check(FORMATS.includes(format), `unknown format '${String(format)}'`);
    if (format === 'deb') check(platform === 'linux-x64', `format deb is not available for platform ${platform}`);
    if (format === 'nsis') check(platform === 'win32-x64', `format nsis is not available for platform ${platform}`);
    if (format === 'app') check(entry.platform === 'darwin', `format app is not available for platform ${platform}`);
    if (format === 'dmg') {
      check(entry.platform === 'darwin', `format dmg is not available for platform ${platform}`);
      check(typeof options.hdiutil === 'function' || process.platform === 'darwin', 'format dmg can be built only on macOS (it needs hdiutil)');
    }
  }
  // Runtime: required regular file, symlink-free, at most 1 GiB.
  check(typeof options.runtime === 'string' && options.runtime !== '', 'runtime binary path is required');
  const runtimePath = path.resolve(options.runtime);
  await noSymlinkComponents(runtimePath);
  const runtimeStat = await lstat(runtimePath);
  check(runtimeStat.isFile(), 'runtime binary must be a regular file');
  check(runtimeStat.size <= GIB, 'runtime binary exceeds the 1 GiB limit');
  const runtimeSize = runtimeStat.size;
  const bytes = await readPackage(path.resolve(packagePath));
  const info = await verify(bytes);
  if (info.status !== 'Signed' && options.allowUnsigned !== true) {
    throw new Error('Bundle: package is unsigned (pass --allow-unsigned to accept)');
  }
  const manifest = readManifestFields(new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(await info.read('alef.ktav')));
  // Stem: explicit --name or a file-name-safe default derived from the manifest.
  let stem;
  if (options.name !== undefined) {
    check(typeof options.name === 'string' && /^[A-Za-z0-9._-]+$/.test(options.name),
      '--name allows only ASCII letters, digits, dot, underscore, dash');
    check(options.name !== '.' && options.name !== '..', "--name may not be '.' or '..'");
    stem = options.name;
  } else {
    stem = `${manifest.name}-${manifest.version}-${platform}`;
    const usable = [...stem].every(char => {
      const code = char.codePointAt(0);
      return code >= 0x20 && code !== 0x7f && !'/\\:*?"<>|'.includes(char);
    }) && !/[ .]$/.test(stem);
    check(usable, `default bundle name '${stem}' is not a safe file name; pass --name`);
  }
  // The launcher is named after the stem inside the bundle root: it must not collide with the fixed entries.
  check(!['app', 'bin', 'bundle.json', 'license', 'notice'].includes(stem.toLowerCase()),
    `'${stem}' collides with a fixed entry of the bundle layout (app, bin, bundle.json, LICENSE, NOTICE); pass another name`);
  // NSIS embeds the stem in Windows paths and shortcut names: ASCII only.
  if (formats.includes('nsis')) {
    check(/^[A-Za-z0-9._-]+$/.test(stem),
      `${stem}: the nsis format requires an ASCII stem (letters, digits, dot, underscore, dash); pass name`);
  }
  // zip entry names must be ASCII: keep the original stem for file names but
  // sanitize a separate prefix used inside the archive.
  const zipStem = stem.replace(/[^A-Za-z0-9._@+-]/g, '_');
  // One executable name for the .app tree: the zip layer needs ASCII, so the
  // zipped app uses the sanitized stem; the dir-only app keeps the raw stem.
  const appExecutable = formats.includes('zip') ? zipStem : stem;
  // Runtime version comes only from the caller: the runtime binary is never executed.
  let version = null;
  if (options.runtimeVersion !== undefined) {
    check(typeof options.runtimeVersion === 'string', 'runtimeVersion must be a string');
    parseVersion(options.runtimeVersion);
    version = options.runtimeVersion;
  } else {
    warnings.push('could not verify the runtime version; pass --runtime-version <semver> to check the package requirement; runtime range not checked');
  }
  if (version !== null && manifest.runtime !== undefined) {
    check(satisfiesRange(version, manifest.runtime),
      `runtime ${version} does not satisfy the package requirement ${manifest.runtime}`);
  }
  const packageSha256 = hash(bytes);
  const runtimeSha256 = await streamHash(runtimePath, runtimeSize);
  // Testing-only barrier seam: awaited once per format publication, right
  // after the preflight/plan checks and before any staging/publication write.
  // The CLI never passes it; tests use it to align concurrent publishers.
  const afterPreflight = options.afterPreflight;
  check(afterPreflight === undefined || typeof afterPreflight === 'function',
    'afterPreflight must be a function or undefined');
  // Output root must be an existing, symlink-free directory.
  const outDir = path.resolve(options.out ?? path.dirname(path.resolve(packagePath)));
  await noSymlinks(outDir);
  check((await lstat(outDir)).isDirectory(), 'output must be a directory');
  // Plan artifacts and refuse any lexical overlap with either input path.
  const appFormat = formats.includes('app');
  const plan = [];
  if (formats.includes('dir')) plan.push({ name: stem, kind: 'dir' });
  if (formats.includes('zip')) plan.push({ name: appFormat ? `${stem}.app.zip` : `${stem}.zip`, kind: 'file' });
  if (formats.includes('tar.gz')) plan.push({ name: `${stem}.tar.gz`, kind: 'file' });
  if (formats.includes('deb')) plan.push({ name: `${stem}.deb`, kind: 'file' });
  if (formats.includes('nsis')) plan.push({ name: `${stem}-nsis`, kind: 'dir' });
  if (appFormat) plan.push({ name: `${stem}.app`, kind: 'dir' });
  if (formats.includes('dmg')) plan.push({ name: `${stem}.dmg`, kind: 'file' });
  for (const artifact of plan) {
    const target = path.join(outDir, artifact.name);
    const inside = within(packagePath, target) ? packagePath : within(runtimePath, target) ? runtimePath : null;
    check(inside === null, `output ${target} collides with the input ${inside}`);
    const wraps = within(target, packagePath) ? packagePath : within(target, runtimePath) ? runtimePath : null;
    check(wraps === null, `output ${target} would contain the input ${wraps}`);
    let exists = false;
    try { await lstat(target); exists = true; } catch (error) { if (error.code !== 'ENOENT') throw error; }
    check(!exists || options.force === true, `${target} already exists (use --force)`);
    // Belt and braces: no planned target may ever leave the output directory.
    check(path.dirname(path.resolve(target)) === outDir, `${target} escapes the output directory`);
  }
  // Shared in-memory layout (the runtime is streamed, never buffered here).
  const files = [{ rel: 'app/app.alef', data: bytes, mode: 0o644 }];
  const runtimeDir = path.dirname(runtimePath);
  for (const basename of (await readdir(runtimeDir)).sort()) {
    if (!/^(license|notice)(\.md|\.txt)?$/i.test(basename)) continue;
    const licenseStat = await lstat(path.join(runtimeDir, basename));
    if (!licenseStat.isFile()) continue; // symlinks and special files are skipped
    files.push({ rel: basename, data: await exactRead(path.join(runtimeDir, basename), LICENSE_LIMIT), mode: 0o644 });
  }
  files.push({
    rel: 'bundle.json',
    data: Buffer.from(`${JSON.stringify({
      id: manifest.id, name: manifest.name, version: manifest.version, platform,
      packageSha256, runtimeSha256, tool: toolVersion(),
    }, null, 2)}\n`, 'utf8'), mode: 0o644,
  });
  // Launcher text never embeds the stem (paths come from %~dp0/$0), so no escaping is needed.
  const wantsLauncher = formats.includes('dir') || formats.includes('tar.gz') || (formats.includes('zip') && !appFormat);
  const launcherName = platform === 'win32-x64' ? `${stem}.cmd` : stem;
  if (wantsLauncher) {
    files.push(platform === 'win32-x64'
      ? { rel: launcherName, data: Buffer.from('@"%~dp0bin\\alef.exe" run "%~dp0app\\app.alef" %*\r\n', 'utf8'), mode: 0o755 }
      : {
          rel: launcherName, mode: 0o755,
          data: Buffer.from('#!/bin/sh\nSELF=$(readlink -f -- "$0" 2>/dev/null || printf \'%s\' "$0")\n'
            + 'DIR=$(CDPATH= cd -- "$(dirname -- "$SELF")" && pwd -P)\n'
            + 'exec "$DIR/bin/alef" run "$DIR/app/app.alef" "$@"\n', 'utf8'),
        });
  }
  // Full runtime bytes are only needed by archive formats that inline them.
  let runtimeData = null;
  const runtimeBytes = async () => {
    if (runtimeData === null) {
      runtimeData = await exactRead(runtimePath, GIB);
      check(runtimeData.length === runtimeSize && hash(runtimeData) === runtimeSha256, 'runtime binary changed while bundling');
    }
    return runtimeData;
  };
  const out = [];
  const staged = async producer => {
    if (afterPreflight) await afterPreflight();
    const stage = path.join(outDir, `.alef-bundle-${randomUUID()}`);
    try {
      await mkdir(stage, { recursive: true, mode: 0o700 });
      const target = await producer(stage);
      await publishDir(stage, target, options.force === true);
      return target;
    } finally { await rm(stage, { recursive: true, force: true }); }
  };
  if (formats.includes('dir')) {
    const target = path.join(outDir, stem);
    await staged(async stage => { await writeLayout(stage, files, runtimePath, runtimeSize, runtimeSha256, entry.binary); return target; });
    out.push(target);
  }
  if (formats.includes('tar.gz')) {
    if (afterPreflight) await afterPreflight();
    const entries = [];
    for (const file of files) entries.push({ name: `${stem}/${file.rel}`, data: file.data, mode: file.mode });
    entries.push({ name: `${stem}/bin/${entry.binary}`, data: await runtimeBytes(), mode: 0o755 });
    await publishFile(path.join(outDir, `${stem}.tar.gz`), writeGzip(writeTar(entries)), options.force === true);
    out.push(path.join(outDir, `${stem}.tar.gz`));
  }
  if (formats.includes('deb')) {
    if (afterPreflight) await afterPreflight();
    const pkg = kebab(manifest.name);
    const hasIcon = info.entries.some(item => item.name === 'icon.png');
    const data = [{ name: `./opt/${pkg}/`, type: '5', data: Buffer.alloc(0), mode: 0o755 }];
    for (const file of files) data.push({ name: `./opt/${pkg}/${file.rel}`, data: file.data, mode: file.mode });
    data.push({ name: `./opt/${pkg}/bin/${entry.binary}`, data: await runtimeBytes(), mode: 0o755 });
    // Debian-only wrapper script so the app resolves through /usr/bin/<pkg>.
    data.push({
      name: `./opt/${pkg}/${pkg}`, mode: 0o755,
      data: Buffer.from(`#!/bin/sh\nexec "/opt/${pkg}/bin/alef" run "/opt/${pkg}/app/app.alef" "$@"\n`, 'utf8'),
    });
    data.push({ name: `./usr/bin/${pkg}`, type: '2', linkname: `/opt/${pkg}/${pkg}`, data: Buffer.alloc(0), mode: 0o755 });
    const totalLayoutBytes = runtimeSize + files.reduce((sum, file) => sum + file.data.length, 0);
    // Key order matches the runtime's desktop-entry writer (render.rs).
    let desktop = '[Desktop Entry]\nType=Application\n'
      + `Name=${escapeDesktopValue(manifest.name)}\n`
      + `Exec=${quoteDesktopExec(`/usr/bin/${pkg}`)}\n`;
    if (hasIcon) desktop += `Icon=${pkg}\n`;
    desktop += 'Terminal=false\n'
      + 'Categories=Utility;\n'
      + `StartupWMClass=${escapeDesktopValue(manifest.id)}\n`;
    data.push({ name: `./usr/share/applications/${pkg}.desktop`, data: Buffer.from(desktop, 'utf8'), mode: 0o644 });
    if (hasIcon) {
      data.push({ name: `./usr/share/icons/hicolor/256x256/apps/${pkg}.png`, data: await info.read('icon.png'), mode: 0o644 });
    }
    await publishFile(path.join(outDir, `${stem}.deb`), buildDeb({
      control: {
        Package: pkg, Version: manifest.version, Architecture: 'amd64',
        Maintainer: options.maintainer ?? 'Alef Bundle <bundle@alef.invalid>',
        Description: `${manifest.name} application packaged with the Alef runtime.`,
        'Installed-Size': String(Math.ceil(totalLayoutBytes / 1024)),
      },
      data,
    }), options.force === true);
    out.push(path.join(outDir, `${stem}.deb`));
  }
  // macOS .app tree; shared by the app format and the combined app+zip format.
  const appTree = async runtimeProvider => {
    const tree = [{
      rel: 'Contents/Info.plist', mode: 0o644,
      data: Buffer.from(renderInfoPlist({
        id: manifest.id, name: manifest.name, executable: appExecutable,
        iconFile: info.entries.some(item => item.name === 'icon.png') ? 'icon.png' : undefined,
      }), 'utf8'),
    }, {
      rel: `Contents/MacOS/${appExecutable}`, mode: 0o755,
      // SELF resolves $0 through its symlink; DIR is the .app root (../..).
      data: Buffer.from('#!/bin/sh\nSELF=$(readlink -f -- "$0" 2>/dev/null || printf \'%s\' "$0")\n'
        + 'DIR=$(CDPATH= cd -- "$(dirname -- "$SELF")/../.." && pwd -P)\n'
        + 'exec "$DIR/Contents/Resources/alef" run "$DIR/Contents/Resources/app.alef" "$@"\n', 'utf8'),
    }, { rel: 'Contents/Resources/app.alef', data: bytes, mode: 0o644 }];
    if (info.entries.some(item => item.name === 'icon.png')) {
      tree.push({ rel: 'Contents/Resources/icon.png', data: await info.read('icon.png'), mode: 0o644 });
    }
    return [...tree, { rel: 'Contents/Resources/alef', data: await runtimeProvider(), mode: 0o755 }];
  };
  if (appFormat) {
    const target = path.join(outDir, `${stem}.app`);
    await staged(async stage => {
      for (const file of await appTree(runtimeBytes)) {
        const fileTarget = path.join(stage, ...file.rel.split('/'));
        await mkdir(path.dirname(fileTarget), { recursive: true });
        await writeFile(fileTarget, file.data, { mode: file.mode });
      }
      return target;
    });
    out.push(target);
  }
  if (formats.includes('dmg')) {
    const target = path.join(outDir, `${stem}.dmg`);
    if (afterPreflight) await afterPreflight();
    const work = path.join(outDir, `.alef-bundle-${randomUUID()}`);
    try {
      await mkdir(work, { mode: 0o700 });
      // The image root is a folder holding the .app: `-srcfolder` copies the CONTENTS of its folder.
      const root = path.join(work, 'root');
      for (const file of await appTree(runtimeBytes)) {
        const fileTarget = path.join(root, `${stem}.app`, ...file.rel.split('/'));
        await mkdir(path.dirname(fileTarget), { recursive: true });
        await writeFile(fileTarget, file.data, { mode: file.mode });
      }
      const image = path.join(work, 'image.dmg');
      await (options.hdiutil ?? runHdiutil)([
        'create', '-srcfolder', root, '-volname', volumeName(manifest.name, stem),
        '-fs', 'HFS+', '-format', 'UDZO', '-ov', image,
      ]);
      const made = await lstat(image).catch(() => null);
      check(made !== null && made.isFile() && made.size > 0, 'hdiutil did not produce the disk image');
      await publishLinked(target, image, options.force === true);
    } finally { await rm(work, { recursive: true, force: true }); }
    out.push(target);
  }
  if (formats.includes('zip')) {
    // The zip layer caps a single entry at 512 MiB: the stored runtime is one entry.
    check(runtimeSize <= ZIP_ENTRY_LIMIT, 'zip entries are limited to 512 MiB; the runtime binary is larger — use tar.gz');
    const source = appFormat ? await appTree(runtimeBytes) : files;
    // Every entry name is sanitized: whole-stem replacement first, then each
    // segment per-character filtered to the ASCII zip charset.
    const zipPrefix = appFormat ? `${zipStem}.app` : zipStem;
    const entries = [];
    for (const file of source) {
      const zipRel = file.rel.split('/')
        .map(segment => segment.replaceAll(stem, zipStem))
        .map(segment => segment.replace(/[^A-Za-z0-9._@+-]/g, '_'))
        .join('/');
      entries.push({ name: `${zipPrefix}/${zipRel}`, data: file.data, mode: file.mode });
    }
    if (!appFormat) entries.push({ name: `${zipStem}/bin/${entry.binary}`, data: await runtimeBytes(), mode: 0o755 });
    const target = path.join(outDir, appFormat ? `${stem}.app.zip` : `${stem}.zip`);
    if (afterPreflight) await afterPreflight();
    await publishFile(target, writeZip(entries), options.force === true);
    out.push(target);
  }
  if (formats.includes('nsis')) {
    const target = path.join(outDir, `${stem}-nsis`);
    await staged(async stage => {
      await writeFile(path.join(stage, 'installer.nsi'), renderNsisInstaller({
        bundleName: stem, displayName: manifest.name, id: manifest.id,
        version: manifest.version, publisher: options.maintainer,
      }), 'utf8');
      await writeFile(path.join(stage, 'build-installer.cmd'), renderBuildInstallerCmd({ bundleName: stem }), 'utf8');
      await writeLayout(stage, files.filter(file => file.rel === 'app/app.alef'), runtimePath, runtimeSize, runtimeSha256, entry.binary);
      return target;
    });
    out.push(target);
  }
  return Object.freeze({
    stem, id: manifest.id, name: manifest.name, version: manifest.version, platform,
    formats: Object.freeze(formats), packageSha256, runtimeSha256,
    out: Object.freeze(out), warnings: Object.freeze(warnings),
  });
}
