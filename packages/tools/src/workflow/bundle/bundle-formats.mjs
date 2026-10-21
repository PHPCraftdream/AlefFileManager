// SPDX-License-Identifier: MIT OR Apache-2.0
// Pure deterministic archive/text generators for bundling: ustar tar, gzip,
// ar, Debian control/deb, plist XML, NSIS installer script, shell escapers.
// No fs access here: callers stage files and run external tools themselves.

import zlib from 'node:zlib';

const fail = message => { throw new Error(`Bundle: ${message}`); };

// Reproducible builds: fixed mtime, overridable via SOURCE_DATE_EPOCH.
const epoch = () => {
  const v = Number(process.env.SOURCE_DATE_EPOCH);
  return Number.isInteger(v) && v >= 0 ? v : 0;
};

const check = (condition, message) => { if (!condition) fail(message); };
const hasControl = value => {
  for (const char of value) {
    const code = char.codePointAt(0);
    if (code < 0x20 || code === 0x7f) return true;
  }
  return false;
};

const octal = (value, width) => value.toString(8).padStart(width - 1, '0') + '\0';

function validateEntryName(name) {
  if (typeof name !== 'string' || name === '') fail('tar: entry name must be a nonempty string');
  if (name.includes('\\')) fail(`tar: backslash in name: ${name}`);
  if (name.startsWith('/') || /^[a-zA-Z]:/.test(name)) fail(`tar: absolute name: ${name}`);
  for (const part of name.split('/')) if (part === '..') fail(`tar: '..' segment in name: ${name}`);
}

/** Ustar POSIX header for one entry; returns the 512-byte header block. */
function tarHeader(entry) {
  validateEntryName(entry.name);
  const bytes = Buffer.from(entry.name, 'utf8');
  let name = bytes; let prefix = null;
  if (bytes.length > 100) {
    const cut = bytes.lastIndexOf('/', 100);
    if (cut < 0 || cut > 155) fail(`tar: name too long: ${entry.name}`);
    name = bytes.subarray(cut + 1);
    prefix = bytes.subarray(0, cut);
    if (name.length === 0 || name.length > 100 || prefix.length > 155) fail(`tar: name too long: ${entry.name}`);
  }
  const type = entry.type ?? '0';
  // '5' is a plain directory entry: empty payload, no data block.
  check(type === '0' || type === '2' || type === '5', `tar: unsupported type '${type}'`);
  const data = entry.data ?? Buffer.alloc(0);
  if (type === '2') {
    check(typeof entry.linkname === 'string' && entry.linkname !== '', `tar: symlink without linkname: ${entry.name}`);
    check(data.length === 0, `tar: symlink with data: ${entry.name}`);
  }
  const header = Buffer.alloc(512, 0);
  header.write(prefix === null ? '' : prefix.toString('utf8'), 345, 155, 'utf8');
  header.write(name.toString('utf8'), 0, 100, 'utf8');
  header.write(octal(entry.mode ?? 0o644, 8), 100, 8, 'utf8');
  header.write(octal(0, 8), 108, 8, 'utf8');
  header.write(octal(0, 8), 116, 8, 'utf8');
  header.write(octal(type === '2' ? 0 : data.length, 12), 124, 12, 'utf8');
  header.write(octal(epoch(), 12), 136, 12, 'utf8');
  header.write(type, 156, 1, 'utf8');
  header.write('ustar\0', 257, 6, 'utf8');
  header.write('00', 263, 2, 'utf8');
  if (type === '2') header.write(entry.linkname, 157, 100, 'utf8');
  header.write('        ', 148, 8, 'utf8');
  let sum = 0;
  for (const byte of header) sum += byte;
  header.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 8, 'utf8');
  return header;
}

/** Ustar archive: 512-byte header + data per entry, padded to 1024 bytes. */
export function writeTar(entries) {
  if (!Array.isArray(entries)) fail('tar: entries must be an array');
  // Canonical order: entries are sorted by UTF-8 name so equal inputs give equal bytes.
  const canonical = [...entries].sort((a, b) => Buffer.compare(Buffer.from(a.name ?? ''), Buffer.from(b.name ?? '')));
  const blocks = [];
  for (const entry of canonical) {
    const data = Buffer.from(entry.data ?? Buffer.alloc(0));
    blocks.push(tarHeader({ ...entry, data }));
    if ((entry.type ?? '0') !== '2') {
      blocks.push(data);
      if (data.length % 512 !== 0) blocks.push(Buffer.alloc(512 - (data.length % 512)));
    }
  }
  blocks.push(Buffer.alloc(1024));
  return Buffer.concat(blocks);
}

/** Deterministic gzip: level 9, mtime 0 (Node default header). */
export function writeGzip(bytes) {
  return zlib.gzipSync(bytes, { level: 9 });
}

/** Classic ar archive with `name/` style member names. */
export function writeAr(members) {
  if (!Array.isArray(members)) fail('ar: members must be an array');
  const parts = [Buffer.from('!<arch>\n', 'utf8')];
  for (const member of members) {
    const { name, data } = member;
    if (typeof name !== 'string' || name === '') fail('ar: member name must be a nonempty string');
    if (name.length + 1 > 16) fail('ar: member name too long');
    const bytes = Buffer.from(data ?? Buffer.alloc(0));
    const header = Buffer.alloc(60, 0x20);
    header.write(`${name}/`, 0, Math.min(name.length + 1, 16), 'utf8');
    header.write('0', 16, 'utf8');
    header.write('0', 28, 'utf8');
    header.write('0', 34, 'utf8');
    header.write('100644', 40, 'utf8');
    header.write(String(bytes.length), 48, 'utf8');
    header.write('`\n', 58, 'utf8');
    parts.push(header, bytes);
    if (bytes.length % 2 === 1) parts.push(Buffer.from('\n', 'utf8'));
  }
  return Buffer.concat(parts);
}

const CONTROL_KEYS = ['Package', 'Version', 'Architecture', 'Maintainer', 'Description', 'Installed-Size'];

function validateControlField(key, value) {
  if (typeof value !== 'string' || value === '') fail(`control: ${key}: must be a nonempty string`);
  if (value.includes('\n') || value.includes('\r')) fail(`control: ${key}: newline in value`);
  if (hasControl(value)) fail(`control: ${key}: control character`);
}

/** Debian control stanza in canonical key order, only provided keys. */
export function renderControl(fields) {
  if (typeof fields !== 'object' || fields === null) fail('control: fields must be an object');
  let text = '';
  for (const key of CONTROL_KEYS) {
    if (!(key in fields)) continue;
    validateControlField(key, fields[key]);
    text += `${key}: ${fields[key]}\n`;
  }
  return text;
}

/** Minimal .deb: ar of debian-binary + control.tar.gz + data.tar.gz. */
export function buildDeb({ control, data }) {
  const fields = control ?? {};
  if (!/^[a-z0-9][a-z0-9+.-]*$/.test(fields.Package ?? '')) fail(`deb: Package: invalid package name '${fields.Package ?? ''}'`);
  const controlTar = writeGzip(writeTar([{ name: './control', data: Buffer.from(renderControl(fields), 'utf8') }]));
  const dataTar = writeGzip(writeTar(data));
  return writeAr([
    { name: 'debian-binary', data: Buffer.from('2.0\n', 'utf8') },
    { name: 'control.tar.gz', data: controlTar },
    { name: 'data.tar.gz', data: dataTar },
  ]);
}

function refuseControl(name, value) {
  check(typeof value === 'string', `${name}: value must be a string`);
  if (hasControl(value)) fail(`${name}: control character`);
  return value;
}

export function escapeXml(value) {
  refuseControl('xml', value);
  return value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;').replaceAll('\'', '&apos;');
}

export function escapeDesktopValue(value) {
  refuseControl('desktop', value);
  return value.replaceAll('\\', '\\\\').replaceAll('\r', '\\r');
}

// Exec line: shell-quote inside double quotes, then Desktop-Entry escape the
// whole quoted result (backslashes double again), matching render.rs exec_token.
const desktopExecInner = value => value.replaceAll('\\', '\\\\').replaceAll('"', '\\"').replaceAll('`', '\\`')
  .replaceAll('$', '\\$').replaceAll('%', '%%');

export function quoteDesktopExec(value) {
  refuseControl('exec', value);
  return '"' + desktopExecInner(value).replaceAll('\\', '\\\\') + '"';
}

export function escapeNsis(value) {
  refuseControl('nsis', value);
  return value.replace(/\$/g, () => '$$').replaceAll('"', '$\\"');
}

// Without these keys macOS kills a process that opens the camera or microphone. Same keys as
// `alef install` (backend/crates/alef/src/runtimes/install/integration/render.rs); the texts speak
// of this one application, not of installed ones.
const USAGE_DESCRIPTIONS = [
  ['NSCameraUsageDescription', 'This application may use the camera; the Alef runtime asks before the application first opens it.'],
  ['NSMicrophoneUsageDescription', 'This application may use the microphone; the Alef runtime asks before the application first opens it.'],
];

/** macOS Info.plist XML for an app bundle. */
export function renderInfoPlist({ id, name, executable, iconFile }) {
  const pairs = [['CFBundleIdentifier', id], ['CFBundleName', name], ['CFBundleExecutable', executable]];
  if (iconFile !== undefined) pairs.push(['CFBundleIconFile', iconFile]);
  pairs.push(...USAGE_DESCRIPTIONS);
  let body = '';
  for (const [key, value] of pairs) {
    check(typeof value === 'string' && value !== '', `plist: ${key}: must be a nonempty string`);
    body += `<key>${escapeXml(key)}</key><string>${escapeXml(value)}</string>`;
  }
  return '<?xml version="1.0" encoding="UTF-8"?>\n'
    + '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
    + '<plist version="1.0">\n<dict>'
    + body
    + '</dict>\n</plist>\n';
}

/** Full NSIS installer script for a per-user Windows install. */
export function renderNsisInstaller({ bundleName, displayName, id, version, publisher }) {
  check(typeof bundleName === 'string' && /^[A-Za-z0-9._-]+$/.test(bundleName), `nsis: bundleName: invalid name '${bundleName ?? ''}'`);
  check(typeof id === 'string' && id !== '', 'nsis: id: must be a nonempty string');
  check(typeof version === 'string' && version !== '', 'nsis: version: must be a nonempty string');
  const name = escapeNsis(refuseControl('nsis: displayName', displayName));
  const appId = escapeNsis(refuseControl('nsis: id', id));
  const safeVersion = escapeNsis(refuseControl('nsis: version', version));
  const publisherLine = publisher === undefined ? ''
    : `WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${bundleName}" "Publisher" "${escapeNsis(refuseControl('nsis: publisher', publisher))}"\n`;
  // Only the quotes are NSIS-escaped (\" -> $\\"); $INSTDIR stays a runtime variable.
  const params = `run ${escapeNsis('"')}$INSTDIR\\app.alef${escapeNsis('"')}`;
  return `; Generated by alef-runtime bundle. Compile: makensis installer.nsi
Name "${name}"
OutFile "${bundleName}-setup.exe"
Unicode true
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\\Programs\\${bundleName}"

Section "Install"
  CreateDirectory "$INSTDIR"
  SetOutPath "$INSTDIR"
  File /oname=alef.exe bin\\alef.exe
  File /oname=app.alef app\\app.alef
  CreateShortcut "$SMPROGRAMS\\${bundleName}.lnk" "$INSTDIR\\alef.exe" "${params}" "$INSTDIR\\alef.exe"
  WriteRegStr HKCU "Software\\Classes\\AppUserModelId\\${appId}" "DisplayName" "${name}"
  WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${bundleName}" "DisplayName" "${name}"
  WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${bundleName}" "DisplayVersion" "${safeVersion}"
${publisherLine}  WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${bundleName}" "DisplayIcon" "$INSTDIR\\alef.exe"
  WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${bundleName}" "UninstallString" '"$INSTDIR\\uninstall.exe"'
  WriteRegDWORD HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${bundleName}" "NoModify" 1
  WriteRegDWORD HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${bundleName}" "NoRepair" 1
  WriteUninstaller "uninstall.exe"
SectionEnd

Section "un.Uninstall"
  Delete "$INSTDIR\\alef.exe"
  Delete "$INSTDIR\\app.alef"
  Delete "$INSTDIR\\uninstall.exe"
  Delete "$SMPROGRAMS\\${bundleName}.lnk"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\\Classes\\AppUserModelId\\${appId}"
  DeleteRegKey HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${bundleName}"
SectionEnd
`;
}

/** One-line build command for the staged installer directory. */
export function renderBuildInstallerCmd({ bundleName }) {
  check(typeof bundleName === 'string' && /^[A-Za-z0-9._-]+$/.test(bundleName), `nsis: bundleName: invalid name '${bundleName ?? ''}'`);
  return 'makensis /NOCD installer.nsi\n';
}
