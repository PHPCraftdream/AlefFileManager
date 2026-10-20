// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { randomBytes, createHash } from 'node:crypto';
import zlib from 'node:zlib';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, mkdir, readdir, rename, symlink, writeFile, readFile, rm, lstat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createKey } from '../src/keys.mjs';
import { pack } from '../src/pack.mjs';
import { bundle } from '../src/workflow/bundle.mjs';
import { readZip } from '../src/zip.mjs';
import { parseRuntimeRange, parseVersion, readManifestFields, satisfiesRange } from '../src/workflow/bundle-manifest.mjs';
import {
  buildDeb, escapeDesktopValue, escapeNsis, escapeXml, quoteDesktopExec, renderBuildInstallerCmd,
  renderControl, renderInfoPlist, renderNsisInstaller, writeAr, writeTar,
} from '../src/workflow/bundle-formats.mjs';

const bounded = { timeout: 5000 };

test('all_four_manifest_fields_are_read_from_a_realistic_ktav_text', bounded, () => {
  const fields = readManifestFields(`id: com.example.app
name:: My App
version:: 1.2.3
runtime:: ^1.2
windows: [
    {
        label: main
        url: /index.html
    }
]
## trailing comment
`);
  assert.deepEqual(fields, { id: 'com.example.app', name: 'My App', version: '1.2.3', runtime: '^1.2' });
  assert.ok(Object.isFrozen(fields));
  assert.equal(readManifestFields('id: a.b\nname:: App\nversion:: 1.0.0\n').runtime, undefined);
});

test('missing_required_duplicate_and_multiline_fields_fail_with_exact_messages', bounded, () => {
  for (const [text, message] of [
    ['name:: App\nversion:: 1.0.0\n', 'Bundle: missing field id'],
    ['id: a.b\nversion:: 1.0.0\n', 'Bundle: missing field name'],
    ['id: a.b\nname:: App\n', 'Bundle: missing field version'],
    ['id: a.b\nid: c.d\nname:: App\nversion:: 1.0.0\n', 'Bundle: id: duplicate field'],
    ['id: a.b\nname:: App\nname:: Other\nversion:: 1.0.0\n', 'Bundle: name: duplicate field'],
    ['id: a.b\nname:: ((multi\nline))\nversion:: 1.0.0\n', 'Bundle: name: multiline values unsupported'],
    ['id: a.b\nname:: App\nruntime:: ((wide\nrange))\nversion:: 1.0.0\n', 'Bundle: runtime: multiline values unsupported'],
  ]) assert.throws(() => readManifestFields(text), error => error.message === message, text);
});

test('ambiguous_tails_non_string_and_malformed_values_are_rejected', bounded, () => {
  for (const [text, message] of [
    ['id: a.b extra\nname:: App\nversion:: 1.0.0\n', 'Bundle: id: ambiguous encoding'],
    ['id: 123\nname:: App\nversion:: 1.0.0\n', 'Bundle: id: must be a string'],
    ['id: a.b\nname: true\nversion:: 1.0.0\n', 'Bundle: name: must be a string'],
    ['id: a.b\nname:: App\nversion: abc\n', 'Bundle: version: invalid semver \'abc'],
    ['id: a.b\nname:: App\nversion: 01.2.3\n', 'Bundle: version: invalid semver \'01.2.3'],
    ['id: -bad\nname:: App\nversion:: 1.0.0\n', 'Bundle: id: invalid identifier \'-bad\''],
    [`id: ${'a'.repeat(256)}\nname:: App\nversion:: 1.0.0\n`, 'Bundle: id: invalid identifier'],
  ]) assert.throws(() => readManifestFields(text), error => error.message.startsWith(message.split('\'')[0]), text);
});

test('id_charset_and_name_control_characters_are_rejected', bounded, () => {
  for (const id of ['com/example', 'com example', '.example', 'example ', 'exämple', 'a$b']) {
    assert.throws(() => readManifestFields(`id: '${id}'\nname:: App\nversion:: 1.0.0\n`), /Bundle: id:/, id);
  }
  // Line-based encoding cannot carry a raw newline in a value; tab/BEL/DEL are caught.
  for (const name of ['bad\tname', 'bad\u0007name', 'bad\u007fname']) {
    assert.throws(() => readManifestFields(`id: a.b\nname:: ${name}\nversion:: 1.0.0\n`), /Bundle: name:/, JSON.stringify(name));
  }
  assert.throws(() => readManifestFields('id: a.b\nname::\nversion:: 1.0.0\n'), /Bundle: name:/);
});

test('caret_tilde_comparator_wildcard_and_exact_ranges_match_the_exact_cargo_boundaries', bounded, () => {
  // Ported from backend runtime/tests.rs:213-246: only vectors INSIDE the strict
  // grammar (requirement, version, expected).
  const vectors = [
    ['^1.2.3', '1.99.0', true], ['^1.2.3', '2.0.0', false], ['^1.2.3', '1.2.2', false],
    ['^1.2.3', '1.2.3', true],
    ['^0.0.3', '0.0.3', true], ['^0.0.3', '0.0.4', false], ['^0.0.3', '0.0.2', false],
    ['~1.2.3', '1.2.99', true], ['~1.2.3', '1.3.0', false], ['~1.2.3', '1.2.2', false],
    ['=0.1.2', '0.1.2', true], ['=0.1.2', '0.1.3', false],
    ['>1.2.3', '1.2.4', true], ['>1.2.3', '1.2.3', false],
    ['>=1.2.3', '1.2.3', true], ['>=1.2.3', '1.2.2', false],
    ['<1.2.3', '1.2.2', true], ['<1.2.3', '1.2.3', false],
    ['<=1.2.3', '1.2.3', true], ['<=1.2.3', '1.2.4', false],
    ['>=1.2.9', '1.2.10', true], ['>=1.2.9', '1.2.8', false],
    ['=1.2.3+one', '1.2.3+two', true], ['=1.2.3+one', '1.2.4+one', false],
    ['>=1.2.3-alpha.1', '1.2.3-alpha.2', true], ['>=1.2.3-alpha.1', '1.2.4-alpha.2', false],
    [' >= 1.2.3 , < 2.0.0 ', '1.5.0', true], [' >= 1.2.3 , < 2.0.0 ', '2.0.0', false],
  ];
  for (const [requirement, version, expected] of vectors) {
    assert.equal(satisfiesRange(version, requirement), expected, `${version} vs ${requirement}`);
  }
  // Contradictory intervals never match.
  assert.equal(satisfiesRange('1.5.0', '>=2.0.0, <1.0.0'), false);
  // BigInt precision: components above 2^53 stay exact.
  assert.equal(satisfiesRange('9007199254740993.0.0', '=9007199254740992.0.0'), false);
  assert.equal(satisfiesRange('9007199254740992.0.0', '=9007199254740992.0.0'), true);
  assert.equal(satisfiesRange('9007199254740993.0.0', '>=9007199254740992.0.0'), true);
  assert.equal(satisfiesRange('9007199254740992.0.0', '>=9007199254740992.0.0'), true);
  // parseVersion: u64 cores are exact; build identifiers are not u64-capped.
  assert.equal(parseVersion('18446744073709551615.0.0').major, 18446744073709551615n);
  assert.doesNotThrow(() => parseVersion('1.2.3+9007199254740993'));
});

test('invalid_range_strings_are_rejected_by_the_parser', bounded, () => {
  // VALID-list entries from backend runtime/tests.rs:8-33 that are INSIDE the
  // strict grammar still parse.
  for (const range of ['~0.1.2', '=0.1.2', '^0.0.3', '>1.2.3', '>=1.2.3', '<1.2.3', '<=1.2.3',
    '=1.2.3-alpha.1', '=1.2.3+build.01', '>=1.2.3-alpha.1, <2.0.0', ' >= 1.2.3 , < 2.0.0 ']) {
    assert.doesNotThrow(() => parseRuntimeRange(range), range);
  }
  // Every other VALID-list entry is now outside the grammar and MUST throw.
  for (const range of ['^0.1', '>=0.1, <0.2', '0.1.*', '0.1.2', '*', 'x', 'X', '1.x', '1.X',
    '1.*.*', '1', '1.2', '~1']) {
    assert.throws(() => parseRuntimeRange(range), error => error.message.startsWith('Bundle: runtime: '), range);
  }
  // The full runtime INVALID list (tests.rs:34-63).
  for (const range of ['', ' ', '\n', '^', '~', '=', '>=', '||', '^0.1 || ^0.2', '>=0.1 <0.2',
    '1.2.3 - 2.0.0', 'v1.2.3', 'latest', '1.2.3.4', '01.2.3', '1.02.3', '1.2.03', '1.2.3-01',
    '1.2.3-', '1.2.3+', '1.2.3-alpha..1', '1.2.3+build..1', '1.2.3-\u00e9', '1.2.3\0', '1.*.3',
    '*, >=1', '>=1,', ',>=1', '>=18446744073709551616.0.0']) {
    assert.throws(() => parseRuntimeRange(range), error => error.message.startsWith('Bundle: runtime: '), range);
  }
  // Grammar-specific refusals: wildcard comparators, bare and partial versions,
  // bare prerelease, and prerelease bounds on caret/tilde.
  for (const range of ['>=*', '^x', '1.2.3-alpha', '>1.2', '<=1.2']) {
    assert.throws(() => parseRuntimeRange(range), error => error.message.startsWith('Bundle: runtime: '), range);
  }
  const preMessage = 'Bundle: runtime: prerelease bounds are only supported for = and comparison operators';
  assert.throws(() => parseRuntimeRange('^1.2.3-alpha'), error => error.message === preMessage);
  assert.throws(() => parseRuntimeRange('~1.2.3-alpha'), error => error.message === preMessage);
  assert.ok(Object.isFrozen(parseRuntimeRange('>=1.2.3, <2.0.0')));
  // parseVersion is the version parser: valid bare versions still parse there.
  assert.doesNotThrow(() => parseVersion('0.1.2'));
  assert.doesNotThrow(() => parseVersion('1.2.3-alpha'));
});

test('invalid_semver_versions_fail_parsing_and_range_checks', bounded, () => {
  // Positive list ported verbatim from backend runtime/tests.rs:250-256.
  for (const version of ['0.0.0', '1.2.3', '1.2.3-alpha.1', '1.2.3+build.01',
    '1.2.3-alpha.1+build.01', '18446744073709551615.0.0']) {
    assert.doesNotThrow(() => parseVersion(version), version);
  }
  // Ported invalid list from backend runtime/tests.rs:263-285.
  const versions = ['', ' ', '1', '1.2', 'v1.2.3', '=1.2.3', '^1.2.3', '1.2.*', '01.2.3', '1.02.3',
    '1.2.03', '1.2.3-01', '1.2.3-', '1.2.3+', '1.2.3-alpha..1', '1.2.3+build..1', '1.2.3-\u00e9',
    '1.2.3\0', ' 1.2.3', '1.2.3 ', '1.2.3.4', '18446744073709551616.0.0'];
  for (const version of versions) {
    assert.throws(() => parseVersion(version), /Bundle: version: invalid semver/, version);
    // Invalid versions never match, without throwing, for any range.
    assert.equal(satisfiesRange(version, '^1.2.3'), false, version);
    assert.equal(satisfiesRange(version, '>=1.0.0, <2.0.0'), false, version);
  }
  // Refused ranges and invalid versions never throw from satisfiesRange.
  for (const range of ['', '   ', '||', '1.2 - 2.0', '>=1.*', '*', '^1', '0.1.2', '>=1.2']) {
    assert.equal(satisfiesRange('1.2.3', range), false, range);
  }
  assert.equal(satisfiesRange('1.2.3-rc', ''), false);
  // A manifest whose runtime is '' is therefore rejected by satisfiesRange.
  const manifest = readManifestFields('id: a.b\nname:: App\nversion:: 1.0.0\nruntime::\n');
  assert.equal(manifest.runtime, '');
  assert.equal(satisfiesRange(manifest.version, manifest.runtime), false);
  const parsed = parseVersion('1.2.3-alpha.1+b7');
  assert.deepEqual({ ...parsed, pre: [...parsed.pre] }, { major: 1n, minor: 2n, patch: 3n, pre: ['alpha', '1'], build: ['b7'] });
  // BigInt components stay exact.
  assert.equal(satisfiesRange('9007199254740992.0.0', '=9007199254740992.0.0'), true);
});

// --- Independent minimal ustar parser for verification (test-only). ---
function parseUstar(buffer) {
  const readStr = (block, offset, length) => block.subarray(offset, offset + length).toString('utf8').replace(/\0.*$/, '');
  const readOct = (block, offset, length) => parseInt(block.subarray(offset, offset + length).toString('utf8').replace(/[\0 ]/g, '').trim(), 8);
  const entries = [];
  let at = 0;
  while (at + 512 <= buffer.length && buffer[at] !== 0) {
    const header = buffer.subarray(at, at + 512);
    let sum = 0;
    for (let i = 0; i < 512; i++) sum += i >= 148 && i < 156 ? 0x20 : header[i];
    assert.equal(sum, readOct(header, 148, 8), 'tar checksum');
    const prefix = readStr(header, 345, 155);
    const size = readOct(header, 124, 12);
    entries.push({
      name: prefix ? `${prefix}/${readStr(header, 0, 100)}` : readStr(header, 0, 100),
      mode: readOct(header, 100, 8), uid: readOct(header, 108, 8), gid: readOct(header, 116, 8), size,
      mtime: readOct(header, 136, 12), magic: readStr(header, 257, 6), version: readStr(header, 263, 2),
      type: readStr(header, 156, 1), linkname: readStr(header, 157, 100),
      data: buffer.subarray(at + 512, at + 512 + size),
    });
    at += 512 + Math.ceil(size / 512) * 512;
  }
  return entries;
}

const tarEpoch = () => { const v = Number(process.env.SOURCE_DATE_EPOCH); return Number.isInteger(v) && v >= 0 ? v : 0; };

test('tar_round_trips_regular_0755_and_symlink_entries_through_an_independent_parser', bounded, () => {
  const data = Buffer.from('hello world');
  const tar = writeTar([
    { name: 'opt/app/file.txt', data },
    { name: 'opt/app/run.sh', data: Buffer.from('#!/bin/sh\n'), mode: 0o755 },
    { name: 'opt/app/latest', type: '2', linkname: 'file.txt', data: Buffer.alloc(0) },
  ]);
  const entries = parseUstar(tar);
  // writeTar writes entries in canonical (UTF-8 name) order.
  assert.deepEqual(entries.map(entry => entry.name), ['opt/app/file.txt', 'opt/app/latest', 'opt/app/run.sh']);
  assert.deepEqual(entries[0], {
    name: 'opt/app/file.txt', mode: 0o644, uid: 0, gid: 0, size: data.length,
    mtime: tarEpoch(), magic: 'ustar', version: '00', type: '0', linkname: '', data,
  });
  assert.equal(entries[2].mode, 0o755);
  assert.equal(entries[1].type, '2');
  assert.equal(entries[1].linkname, 'file.txt');
  assert.equal(entries[1].size, 0);
  const tail = tar.subarray(tar.length - 1024);
  assert.ok(tail.every(byte => byte === 0), 'final padding is at least two zero blocks');
});

test('tar_refuses_unsplittable_long_names_absolute_paths_and_dotdot_segments', bounded, () => {
  assert.throws(
    () => writeTar([{ name: `${'a'.repeat(156)}/b`, data: Buffer.alloc(0) }]),
    error => error.message === `Bundle: tar: name too long: ${'a'.repeat(156)}/b`,
  );
  for (const name of ['/etc/passwd', 'C:/x', 'a/../b']) {
    assert.throws(() => writeTar([{ name, data: Buffer.alloc(0) }]), /Bundle: tar: /, name);
  }
  // Long name that DOES split at a slash boundary inside the first 155 bytes is accepted.
  const split = `${'a'.repeat(50)}/${'b'.repeat(90)}`;
  assert.equal(parseUstar(writeTar([{ name: split, data: Buffer.alloc(0) }]))[0].name, split);
});

test('ar_round_trips_two_members_including_an_odd_sized_one_through_an_independent_parser', bounded, () => {
  const odd = Buffer.from('abc');
  const even = Buffer.from('de');
  const ar = writeAr([{ name: 'first.txt', data: odd }, { name: 'second.txt', data: even }]);
  assert.equal(ar.subarray(0, 8).toString(), '!<arch>\n');
  let at = 8;
  const parsed = [];
  while (at < ar.length) {
    const header = ar.subarray(at, at + 60);
    const name = header.subarray(0, 16).toString().replace(/\/\s*$/, '');
    const size = parseInt(header.subarray(48, 58).toString().trim(), 10);
    assert.equal(header.subarray(58, 60).toString(), '`\n');
    assert.equal(header.subarray(16, 28).toString().trim(), '0', 'mtime is 0');
    assert.equal(header.subarray(40, 46).toString(), '100644');
    parsed.push({ name, data: ar.subarray(at + 60, at + 60 + size).toString() });
    at += 60 + size + (size % 2);
  }
  assert.deepEqual(parsed, [{ name: 'first.txt', data: 'abc' }, { name: 'second.txt', data: 'de' }]);
  assert.equal(at, ar.length, 'odd member was padded by one byte');
  assert.throws(() => writeAr([{ name: 'x'.repeat(16), data: Buffer.alloc(0) }]), /Bundle: ar: member name too long/);
});

test('control_renders_canonical_order_and_rejects_newlines_and_control_characters', bounded, () => {
  assert.equal(
    renderControl({ Description: 'Sync app', Architecture: 'all', Package: 'alef-sync' }),
    'Package: alef-sync\nArchitecture: all\nDescription: Sync app\n',
  );
  assert.throws(() => renderControl({ Package: 'a\nb' }), error => error.message === 'Bundle: control: Package: newline in value');
  assert.throws(() => renderControl({ Version: '1.0\u0007' }), error => error.message === 'Bundle: control: Version: control character');
  assert.throws(() => renderControl({ Package: '' }), /Bundle: control: Package:/);
  assert.throws(() => renderControl({ Maintainer: 5 }), /Bundle: control: Maintainer:/);
});

// --- Independent minimal ar reader for deb verification. ---
function parseAr(buffer) {
  assert.equal(buffer.subarray(0, 8).toString(), '!<arch>\n');
  const members = [];
  let at = 8;
  while (at < buffer.length) {
    const size = parseInt(buffer.subarray(at + 48, at + 58).toString().trim(), 10);
    members.push({
      name: buffer.subarray(at, at + 16).toString().replace(/\/\s*$/, ''),
      data: Buffer.from(buffer.subarray(at + 60, at + 60 + size)),
    });
    at += 60 + size + (size % 2);
  }
  return members;
}

test('build_deb_produces_an_ar_with_three_members_and_embedded_control_tarball', bounded, () => {
  const control = { Package: 'alef-sync', Version: '1.2.3', Architecture: 'all', Maintainer: 'Alef <dev@alef.dev>', Description: 'Sync app' };
  const deb = buildDeb({ control, data: [{ name: './opt/alef/app.alef', data: Buffer.from('x') }] });
  const members = parseAr(deb);
  assert.deepEqual(members.map(m => m.name), ['debian-binary', 'control.tar.gz', 'data.tar.gz']);
  assert.equal(members[0].data.toString(), '2.0\n');
  const controlTar = zlib.gunzipSync(members[1].data);
  const controlEntries = parseUstar(controlTar);
  assert.equal(controlEntries.length, 1);
  assert.equal(controlEntries[0].name, './control');
  const controlText = controlTar.subarray(512, 512 + controlEntries[0].size).toString('utf8');
  assert.equal(controlText, renderControl(control));
  assert.ok(zlib.gunzipSync(members[2].data).length > 0);
  for (const pkg of ['', 'Alef', '-bad', 'a b', 'a/b']) {
    assert.throws(() => buildDeb({ control: { Package: pkg }, data: [] }), /Bundle: deb: Package:/, pkg);
  }
});

test('plist_renders_exact_text_with_and_without_icon_and_escapes_hostile_values', bounded, () => {
  const usage = '<key>NSCameraUsageDescription</key><string>This application may use the camera; the Alef runtime asks before the application first opens it.</string>'
    + '<key>NSMicrophoneUsageDescription</key><string>This application may use the microphone; the Alef runtime asks before the application first opens it.</string>';
  const expected = '<?xml version="1.0" encoding="UTF-8"?>\n'
    + '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
    + '<plist version="1.0">\n<dict>'
    + '<key>CFBundleIdentifier</key><string>dev.alef.sync</string>'
    + '<key>CFBundleName</key><string>Alef</string>'
    + '<key>CFBundleExecutable</key><string>alef</string>'
    + usage
    + '</dict>\n</plist>\n';
  assert.equal(renderInfoPlist({ id: 'dev.alef.sync', name: 'Alef', executable: 'alef' }), expected);
  assert.equal(
    renderInfoPlist({ id: 'dev.alef.sync', name: 'Alef', executable: 'alef', iconFile: 'alef.icns' }),
    expected.replace('<key>CFBundleExecutable</key><string>alef</string>',
      '<key>CFBundleExecutable</key><string>alef</string><key>CFBundleIconFile</key><string>alef.icns</string>'),
  );
  assert.equal(
    renderInfoPlist({ id: 'a&b<c>"d\'e', name: 'Alef', executable: 'alef' }),
    expected.replace('<string>dev.alef.sync</string>', '<string>a&amp;b&lt;c&gt;&quot;d&apos;e</string>'),
  );
  assert.throws(() => renderInfoPlist({ id: 'a\u0000b', name: 'A', executable: 'a' }), /Bundle: xml: control character/);
});

test('nsis_installer_contains_install_dir_shortcut_aumid_and_uninstall_keys_with_escaped_values', bounded, () => {
  const nsi = renderNsisInstaller({
    bundleName: 'alef-sync', displayName: 'Alef "Sync" $1', id: 'dev.alef.sync', version: '1.2.3', publisher: 'Alef',
  });
  assert.ok(nsi.startsWith('; Generated by alef-runtime bundle. Compile: makensis installer.nsi\n'));
  assert.ok(nsi.includes('InstallDir "$LOCALAPPDATA\\Programs\\alef-sync"'), nsi);
  assert.ok(nsi.includes('Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\alef-sync'), nsi);
  assert.ok(nsi.includes('Software\\Classes\\AppUserModelId\\dev.alef.sync'), nsi);
  assert.ok(nsi.includes('OutFile "alef-sync-setup.exe"'), nsi);
  // Shortcut parameters are NSIS-escaped: quotes via escapeNsis, $INSTDIR intact.
  const params = `run ${escapeNsis('"')}$INSTDIR\\app.alef${escapeNsis('"')}`;
  const shortcut = `CreateShortcut "$SMPROGRAMS\\alef-sync.lnk" "$INSTDIR\\alef.exe" "${params}" "$INSTDIR\\alef.exe"`;
  assert.ok(nsi.includes(shortcut), nsi);
  // Hard literal pin: inner double quotes must be NSIS-escaped as $\".
  assert.ok(nsi.includes('run $\\"$INSTDIR\\app.alef$\\"'), nsi);
  assert.ok(nsi.includes('Delete "$SMPROGRAMS\\alef-sync.lnk"'), nsi);
  // The display name appears only in Name/AUMID/uninstall values, never in .lnk paths.
  for (const line of nsi.split('\n')) {
    if (line.includes('.lnk')) assert.ok(!line.includes('Sync'), line);
  }
  assert.ok(nsi.includes('Name "Alef $\\"Sync$\\" $$1"'), nsi);
  assert.ok(nsi.includes('"Publisher" "Alef"'), nsi);
  assert.ok(nsi.includes('"UninstallString" \'"$INSTDIR\\uninstall.exe"\''), nsi);
  assert.ok(nsi.includes('"NoModify" 1') && nsi.includes('"NoRepair" 1'));
  assert.ok(nsi.includes('WriteUninstaller "uninstall.exe"'));
  const noPublisher = renderNsisInstaller({ bundleName: 'a', displayName: 'A', id: 'x.y', version: '1.0.0' });
  assert.ok(!noPublisher.includes('"Publisher"'));
  assert.throws(
    () => renderNsisInstaller({ bundleName: 'bad name', displayName: 'A', id: 'x.y', version: '1.0.0' }),
    /Bundle: nsis: bundleName: invalid name/,
  );
  assert.equal(renderBuildInstallerCmd({ bundleName: 'alef-sync' }), 'makensis /NOCD installer.nsi\n');
  assert.throws(() => renderBuildInstallerCmd({ bundleName: 'a$b' }), /Bundle: nsis: bundleName:/);
});

test('escapers_produce_exact_outputs_and_reject_control_characters', bounded, () => {
  const xml = [['a&b', 'a&amp;b'], ['<x>', '&lt;x&gt;'], ['"', '&quot;'], ["'", '&apos;'], ['plain', 'plain']];
  for (const [input, output] of xml) assert.equal(escapeXml(input), output, input);
  const desktop = [['a\\b', 'a\\\\b'], ['plain', 'plain']];
  for (const [input, output] of desktop) assert.equal(escapeDesktopValue(input), output, input);
  // Two layers, mirroring render.rs exec_token: shell-quote with inner escapes,
  // then Desktop-Entry escape doubles every backslash again.
  const exec = [
    ['a"b', '"a\\\\"b"'], // inner "a\"b" -> backslashes doubled
    ['a$b', '"a\\\\$b"'],
    ['a%b', '"a%%b"'],
    ['a\\b', '"a\\\\\\\\b"'],
    ['a`b', '"a\\\\`b"'], // inner backtick escape then the backslash doubles
    ['x', '"x"'],
  ];
  for (const [input, output] of exec) assert.equal(quoteDesktopExec(input), output, input);
  // Literal from backend render.rs desktop_entry_escapes_hostile_values_without_field_substitution.
  assert.equal(quoteDesktopExec('/apps/a $b%/run"now'), '"/apps/a \\\\$b%%/run\\\\"now"');
  const nsis = [['a$b', 'a$$b'], ['a"b', 'a$\\"b'], ['C:\\path', 'C:\\path']];
  for (const [input, output] of nsis) assert.equal(escapeNsis(input), output, input);
  for (const escape of [escapeXml, escapeDesktopValue, quoteDesktopExec, escapeNsis]) {
    for (const hostile of ['a\u0001b', 'a\tb', 'a\nb', 'a\u007fb']) {
      assert.throws(() => escape(hostile), /Bundle: .*: control character/, JSON.stringify(hostile));
    }
  }
});

// --- End-to-end bundle() tests. ---

const bundleBounded = { timeout: 30000 };
async function bundleScratch(run) {
  const root = await mkdtemp(path.join(tmpdir(), 'alef-bundle-'));
  try { return await run(root); } finally { await rm(root, { recursive: true, force: true }); }
}
// Realistic manifest modeled on create-alef-runtime's base template, signed with a
// scratch key home; the runtime binary is a few KB of random trusted bytes.
async function makeFixture(root, { name = 'Sample App', id = 'com.example.app', runtimeRange = '^1.2.3', sign = true, icon = false, runtimeData = randomBytes(4096), home = null } = {}) {
  const app = path.join(root, 'app');
  await mkdir(path.join(app, 'dist'), { recursive: true });
  if (home === null) { home = path.join(root, 'home'); await mkdir(home, { recursive: true }); await createKey(undefined, { home }); }
  await writeFile(path.join(app, 'alef.ktav'),
    `id: ${id}\nname:: ${name}\nversion:: 1.2.3\nruntime:: ${runtimeRange}\nwindows: [\n    {\n        label: main\n        url: /index.html\n    }\n]\nexternal: {\n    connect: []\n    load: {\n        scripts: []\n        styles: []\n        images: []\n        fonts: []\n        media: []\n        frames: []\n    }\n}\npermissions: {\n    fs: {\n        read: []\n        write: []\n    }\n    cli: {\n        exec: []\n    }\n    net: {\n        http: []\n        socket: []\n    }\n    shell: {\n        openExternal: []\n    }\n    clipboard: {\n        read: false\n    }\n    shortcut: {\n        global: false\n    }\n    secrets: false\n    tray: false\n    window: {\n        create: false\n    }\n    app: {\n        env: []\n        autostart: false\n    }\n}\n`);
  await writeFile(path.join(app, 'dist', 'index.html'), '<html><body>Sample App</body></html>\n');
  if (icon) await writeFile(path.join(app, 'icon.png'), Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3]));
  const packagePath = path.join(root, 'pkg.alef');
  await pack(app, sign ? { out: packagePath, home } : { out: packagePath, home, unsigned: true });
  const runtime = path.join(root, 'runtime');
  await writeFile(runtime, runtimeData);
  return { packagePath, runtime, runtimeBytes: runtimeData };
}
const fileHash = bytes => createHash('sha256').update(bytes).digest('hex');
const shLauncher = '#!/bin/sh\nSELF=$(readlink -f -- "$0" 2>/dev/null || printf \'%s\' "$0")\n'
  + 'DIR=$(CDPATH= cd -- "$(dirname -- "$SELF")" && pwd -P)\nexec "$DIR/bin/alef" run "$DIR/app/app.alef" "$@"\n';
const appLauncher = '#!/bin/sh\nSELF=$(readlink -f -- "$0" 2>/dev/null || printf \'%s\' "$0")\n'
  + 'DIR=$(CDPATH= cd -- "$(dirname -- "$SELF")/../.." && pwd -P)\n'
  + 'exec "$DIR/Contents/Resources/alef" run "$DIR/Contents/Resources/app.alef" "$@"\n';
const posixMode = async file => process.platform === 'win32' ? undefined : ((await lstat(file)).mode & 0o777);
// Minimal ZIP central-directory walker: name plus external-attribute high word.
function parseZipCentral(buffer) {
  const eocd = buffer.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  assert.ok(eocd >= 0, 'missing EOCD');
  const count = buffer.readUInt16LE(eocd + 10);
  let at = buffer.readUInt32LE(eocd + 16);
  const entries = [];
  for (let index = 0; index < count; index++) {
    assert.equal(buffer.readUInt32LE(at), 0x02014b50, 'central signature');
    const nameLength = buffer.readUInt16LE(at + 28);
    entries.push({
      name: buffer.subarray(at + 46, at + 46 + nameLength).toString('ascii'),
      mode: buffer.readUInt32LE(at + 38) >>> 16,
    });
    at += 46 + nameLength + buffer.readUInt16LE(at + 30) + buffer.readUInt16LE(at + 32);
  }
  return entries;
}

test('dir_layout_copies_runtime_and_exact_package_bytes_and_writes_a_correct_bundle_json_and_launcher', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime, runtimeBytes } = await makeFixture(root);
  // A LICENSE next to the runtime is carried into the layout root verbatim.
  const licenseBytes = Buffer.from(Array.from({ length: 300 }, (_, index) => (index * 7) & 255));
  await writeFile(path.join(root, 'LICENSE'), licenseBytes);
  const result = await bundle(packagePath, { runtime, platform: 'linux-x64', formats: ['dir'] });
  assert.equal(result.stem, 'Sample App-1.2.3-linux-x64');
  const stemDir = path.join(root, 'Sample App-1.2.3-linux-x64');
  assert.deepEqual([...result.out], [stemDir]);
  assert.ok(Object.isFrozen(result) && Object.isFrozen(result.out) && Object.isFrozen(result.warnings));
  assert.deepEqual(await readFile(path.join(stemDir, 'bin', 'alef')), runtimeBytes);
  assert.deepEqual(await readFile(path.join(stemDir, 'app', 'app.alef')), await readFile(packagePath));
  const meta = JSON.parse(await readFile(path.join(stemDir, 'bundle.json'), 'utf8'));
  assert.deepEqual(Object.keys(meta), ['id', 'name', 'version', 'platform', 'packageSha256', 'runtimeSha256', 'tool']);
  assert.equal(meta.id, 'com.example.app'); assert.equal(meta.name, 'Sample App');
  assert.equal(meta.version, '1.2.3'); assert.equal(meta.platform, 'linux-x64');
  assert.equal(meta.packageSha256, fileHash(await readFile(packagePath)));
  assert.equal(meta.runtimeSha256, fileHash(runtimeBytes));
  assert.ok(meta.tool.length > 0);
  const launcher = path.join(stemDir, 'Sample App-1.2.3-linux-x64');
  assert.equal(await readFile(launcher, 'utf8'), shLauncher);
  const launcherMode = await posixMode(launcher);
  if (launcherMode !== undefined) assert.equal(launcherMode, 0o755);
  const binMode = await posixMode(path.join(stemDir, 'bin', 'alef'));
  if (binMode !== undefined) assert.equal(binMode, 0o755);
  assert.deepEqual(await readFile(path.join(stemDir, 'LICENSE')), licenseBytes);
  await assert.rejects(lstat(path.join(stemDir, 'NOTICE')));
  // The explicit --name variant overrides the space-bearing default stem.
  const named = await bundle(packagePath, { runtime, platform: 'linux-x64', formats: ['dir'], name: 'sample' });
  assert.equal(named.stem, 'sample');
  assert.equal(await readFile(path.join(root, 'sample', 'bin', 'alef')).then(b => b.length), runtimeBytes.length);
}));
test('windows_dir_layout_uses_alef_exe_and_the_exact_crlf_cmd_launcher_text', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  const result = await bundle(packagePath, { runtime, platform: 'win32-x64', formats: ['dir'], name: 'sample' });
  const stemDir = path.join(root, 'sample');
  assert.deepEqual([...result.out], [stemDir]);
  assert.ok((await lstat(path.join(stemDir, 'bin', 'alef.exe'))).isFile());
  const cmd = await readFile(path.join(stemDir, 'sample.cmd'));
  assert.deepEqual(cmd, Buffer.from('@"%~dp0bin\\alef.exe" run "%~dp0app\\app.alef" %*\r\n', 'utf8'));
  assert.ok(!cmd.includes(Buffer.from('sample')));
}));
test('zip_and_tar_gz_archives_are_deterministic_and_round_trip_with_expected_names_and_modes', bundleBounded, () => bundleScratch(async rootA => {
  const sharedHome = path.join(rootA, 'shared-home');
  await mkdir(sharedHome, { recursive: true }); await createKey(undefined, { home: sharedHome });
  const build = async root => {
    const bytes = Buffer.from(Array.from({ length: 4096 }, (_, index) => (index * 31 + 7) & 255));
    const { packagePath, runtime } = await makeFixture(root, { runtimeData: bytes, home: sharedHome });
    const result = await bundle(packagePath, { runtime, platform: 'linux-x64', formats: ['zip', 'tar.gz'], name: 'sample' });
    return {
      zip: await readFile(result.out.find(file => file.endsWith('.zip'))),
      tar: await readFile(result.out.find(file => file.endsWith('.tar.gz'))),
    };
  };
  const first = await bundleScratch(build); const second = await bundleScratch(build);
  assert.deepEqual(first.zip, second.zip);
  assert.deepEqual(first.tar, second.tar);
  const archive = await readZip(first.zip);
  assert.deepEqual(archive.entries.map(entry => entry.name),
    ['sample/app/app.alef', 'sample/bin/alef', 'sample/bundle.json', 'sample/sample']);
  // Modes ride in the external-attribute high word of the central directory.
  const zipEntries = parseZipCentral(first.zip);
  const zipMode = name => zipEntries.find(entry => entry.name === name).mode;
  assert.equal(zipMode('sample/bin/alef'), 0o100755);
  assert.equal(zipMode('sample/sample'), 0o100755);
  assert.equal(zipMode('sample/app/app.alef'), 0o100644);
  assert.equal(zipMode('sample/bundle.json'), 0o100644);
  const entries = parseUstar(zlib.gunzipSync(first.tar));
  assert.equal(entries.find(entry => entry.name === 'sample/bin/alef').mode, 0o755);
  assert.equal(entries.find(entry => entry.name === 'sample/sample').mode, 0o755);
  assert.equal(entries.find(entry => entry.name === 'sample/app/app.alef').mode, 0o644);
}));
test('hostile_manifest_names_are_escaped_correctly_in_desktop_plist_and_nsis_outputs', bundleBounded, () => bundleScratch(async root => {
  const hostile = 'He $said "hi" & 100% <b>';
  const { packagePath, runtime } = await makeFixture(root, { name: hostile, id: 'com.example.h-i' });
  await bundle(packagePath, { runtime, platform: 'linux-x64', formats: ['deb'], name: 'hostile' });
  const members = parseAr(await readFile(path.join(root, 'hostile.deb')));
  const dataTar = zlib.gunzipSync(members.find(member => member.name === 'data.tar.gz').data);
  const desktopEntry = parseUstar(dataTar).find(entry => entry.name === './usr/share/applications/he-said-hi-100-b.desktop');
  const desktop = dataTar.subarray(dataTar.indexOf(Buffer.from('[Desktop Entry]')), dataTar.indexOf(Buffer.from('[Desktop Entry]')) + 512).toString('utf8');
  assert.ok(desktopEntry !== undefined);
  const text = desktop.split('\0')[0];
  // Desktop Entry Name is a plain string: only backslash/CR are escaped, so the
  // raw quotes, ampersand and angle brackets stay literal per the spec.
  assert.ok(text.includes('Name=He $said "hi" & 100% <b>'), text);
  assert.ok(text.includes('Exec="/usr/bin/he-said-hi-100-b"'), text);
  assert.ok(text.includes('StartupWMClass=com.example.h-i'), text);
  assert.ok(text.includes('Terminal=false') && text.includes('Categories=Utility;'), text);
  assert.ok(!text.includes('Version=1.0'), text);
  await bundle(packagePath, { runtime, platform: 'darwin-arm64', formats: ['app'], name: 'hostile' });
  const plist = await readFile(path.join(root, 'hostile.app', 'Contents', 'Info.plist'), 'utf8');
  assert.ok(plist.includes('<string>He $said &quot;hi&quot; &amp; 100% &lt;b&gt;</string>'), plist);
  await bundle(packagePath, { runtime, platform: 'win32-x64', formats: ['nsis'], name: 'hostile' });
  const nsi = await readFile(path.join(root, 'hostile-nsis', 'installer.nsi'), 'utf8');
  assert.ok(nsi.includes('Name "He $$said $\\\"hi$\\\" & 100% <b>"'), nsi);
  assert.ok(nsi.includes('OutFile "hostile-setup.exe"'), nsi);
  assert.ok(nsi.includes('CreateShortcut "$SMPROGRAMS\\hostile.lnk"'), nsi);
  assert.ok(nsi.includes('Delete "$SMPROGRAMS\\hostile.lnk"'), nsi);
}));
test('deb_output_is_byte_identical_across_runs', bundleBounded, () => bundleScratch(async rootA => {
  const sharedHome = path.join(rootA, 'shared-home');
  await mkdir(sharedHome, { recursive: true }); await createKey(undefined, { home: sharedHome });
  const build = async root => {
    const bytes = Buffer.from(Array.from({ length: 4096 }, (_, index) => (index * 17 + 3) & 255));
    const { packagePath, runtime } = await makeFixture(root, { runtimeData: bytes, home: sharedHome });
    const result = await bundle(packagePath, { runtime, platform: 'linux-x64', formats: ['deb'], name: 'sample' });
    return readFile(result.out[0]);
  };
  const first = await bundleScratch(build); const second = await bundleScratch(build);
  assert.deepEqual(first, second);
  const members = parseAr(first);
  const dataTar = zlib.gunzipSync(members.find(member => member.name === 'data.tar.gz').data);
  const entries = parseUstar(dataTar);
  assert.ok(entries.some(entry => entry.name === './opt/sample-app/'));
  assert.equal(entries.find(entry => entry.name === './usr/bin/sample-app').type, '2');
  assert.equal(entries.find(entry => entry.name === './usr/bin/sample-app').linkname, '/opt/sample-app/sample-app');
  const wrapper = entries.find(entry => entry.name === './opt/sample-app/sample-app');
  assert.ok(wrapper !== undefined && wrapper.mode === 0o755, JSON.stringify(entries));
  assert.ok(wrapper.data.toString().startsWith('#!/bin/sh\nexec "/opt/sample-app/bin/alef" run "/opt/sample-app/app/app.alef"'), wrapper.data.toString());
}));
test('bundle_refuses_unavailable_formats_unsigned_packages_bad_names_and_existing_or_unsafe_outputs', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  const base = { runtime, formats: ['dir'], name: 'sample' };
  await assert.rejects(bundle(packagePath, { ...base, platform: 'win32-x64', formats: ['deb'] }),
    error => error.message === 'Bundle: format deb is not available for platform win32-x64');
  await assert.rejects(bundle(packagePath, { ...base, platform: 'darwin-arm64', formats: ['nsis'] }),
    error => error.message === 'Bundle: format nsis is not available for platform darwin-arm64');
  const unsigned = await makeFixture(path.join(root, 'u'), { sign: false });
  await assert.rejects(bundle(unsigned.packagePath, { runtime: unsigned.runtime, platform: 'linux-x64', formats: ['dir'], name: 'sample' }),
    error => error.message === 'Bundle: package is unsigned (pass --allow-unsigned to accept)');
  const accepted = await bundle(unsigned.packagePath, { runtime: unsigned.runtime, platform: 'linux-x64', formats: ['dir'], name: 'sample', allowUnsigned: true });
  assert.equal(accepted.out.length, 1);
  await assert.rejects(bundle(packagePath, { ...base, platform: 'linux-x64', name: 'bad/name' }),
    error => error.message === 'Bundle: --name allows only ASCII letters, digits, dot, underscore, dash');
  await bundle(packagePath, { ...base, platform: 'linux-x64' });
  const dup = path.join(root, 'sample');
  await assert.rejects(bundle(packagePath, { ...base, platform: 'linux-x64' }),
    error => error.message === `Bundle: ${dup} already exists (use --force)`);
  const forced = await bundle(packagePath, { ...base, platform: 'linux-x64', force: true });
  assert.deepEqual([...forced.out], [dup]);
  const strict = await makeFixture(path.join(root, 's'), { runtimeRange: '^9.0.0' });
  await assert.rejects(bundle(strict.packagePath, { runtime: strict.runtime, platform: 'linux-x64', formats: ['dir'], name: 'sample', runtimeVersion: '1.0.0' }),
    error => error.message === 'Bundle: runtime 1.0.0 does not satisfy the package requirement ^9.0.0');
  const ok = await bundle(packagePath, { ...base, platform: 'linux-x64', runtimeVersion: '1.2.9', name: 'vok' });
  assert.equal(ok.out.length, 1);
  await assert.rejects(bundle(packagePath, { ...base, platform: 'linux-x64', runtimeVersion: '1.2' }),
    error => error.message.startsWith('Bundle: version: invalid semver'));
  await assert.rejects(bundle(packagePath, { ...base, platform: 'linux-x64', out: packagePath }),
    error => error.message === 'Bundle: output must be a directory');
  // Dot names are refused explicitly, with and without force.
  for (const name of ['.', '..']) {
    await assert.rejects(bundle(packagePath, { ...base, platform: 'linux-x64', name }),
      error => error.message === "Bundle: --name may not be '.' or '..'");
    await assert.rejects(bundle(packagePath, { ...base, platform: 'linux-x64', name, force: true }),
      error => error.message === "Bundle: --name may not be '.' or '..'");
  }
  // A stem that would place the output at the runtime path itself is refused.
  await assert.rejects(bundle(packagePath, { ...base, platform: 'linux-x64', out: root, name: 'runtime' }),
    error => /Bundle: output .* collides with the input .*/.test(error.message));
}));
test('cross_platform_bundles_warn_that_the_runtime_version_was_not_checked_and_still_succeed', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  // No runtimeVersion: exactly one warning on every host platform.
  for (const platform of ['linux-x64', 'win32-x64', 'darwin-arm64', 'darwin-x64']) {
    const result = await bundle(packagePath, { runtime, platform, formats: ['dir'], name: platform });
    assert.equal(result.warnings.length, 1, platform);
    assert.ok(result.warnings[0].endsWith('runtime range not checked'), platform);
  }
}));
test('nsis_staging_dir_contains_the_installer_script_build_cmd_and_real_payload_bytes', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime, runtimeBytes } = await makeFixture(root);
  const result = await bundle(packagePath, { runtime, platform: 'win32-x64', formats: ['nsis'], name: 'sample' });
  const stageDir = path.join(root, 'sample-nsis');
  assert.deepEqual([...result.out], [stageDir]);
  const nsi = await readFile(path.join(stageDir, 'installer.nsi'), 'utf8');
  assert.ok(nsi.includes('InstallDir "$LOCALAPPDATA\\Programs\\sample"'), nsi);
  assert.ok(nsi.includes('Software\\Classes\\AppUserModelId\\com.example.app'), nsi);
  assert.ok(nsi.includes('Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\sample'), nsi);
  assert.equal(await readFile(path.join(stageDir, 'build-installer.cmd'), 'utf8'), renderBuildInstallerCmd({ bundleName: 'sample' }));
  assert.deepEqual(await readFile(path.join(stageDir, 'bin', 'alef.exe')), runtimeBytes);
  assert.deepEqual(await readFile(path.join(stageDir, 'app', 'app.alef')), await readFile(packagePath));
  assert.ok(await lstat(path.join(stageDir, 'installer.nsi')).then(stat => stat.isFile()));
}));
// The real compiler against the staged script. CI turns it on (NSIS installed on Linux and
// Windows runners); it never runs the installer itself, which stays a manual check.
test('makensis_compiles_the_staged_installer_into_a_setup_exe_without_warnings', {
  timeout: 120000,
  skip: process.env.ALEF_TEST_MAKENSIS === '1' ? false : 'ALEF_TEST_MAKENSIS=1 is not set',
}, () => bundleScratch(async root => {
  const { packagePath, runtime, runtimeBytes } = await makeFixture(root);
  await bundle(packagePath, { runtime, platform: 'win32-x64', formats: ['nsis'], name: 'sample' });
  const stageDir = path.join(root, 'sample-nsis');
  const { stdout } = await promisify(execFile)(process.env.MAKENSIS || 'makensis', ['-NOCD', 'installer.nsi'], { cwd: stageDir, timeout: 90000 });
  assert.doesNotMatch(stdout, /warning:/i, stdout);
  const setup = await readFile(path.join(stageDir, 'sample-setup.exe'));
  assert.equal(setup.subarray(0, 2).toString('latin1'), 'MZ');
  assert.ok(setup.length > runtimeBytes.length, `setup is ${setup.length} bytes`);
}));
// The dmg tool is injected: the real hdiutil exists only on macOS. The fake records its arguments and
// what the image root held at that moment, then writes the "image" at the output path (the last argument).
function fakeHdiutil(calls) {
  return async args => {
    const source = args[args.indexOf('-srcfolder') + 1];
    calls.push({ args, plist: await readFile(path.join(source, 'sample.app', 'Contents', 'Info.plist'), 'utf8') });
    await writeFile(args.at(-1), 'FAKE-DMG');
  };
}
const leftovers = async root => (await readdir(root)).filter(name => name.startsWith('.alef-bundle'));
test('dmg_hands_the_tool_an_image_root_holding_the_app_and_publishes_the_image_the_tool_made', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  const calls = [];
  const result = await bundle(packagePath, { runtime, platform: 'darwin-arm64', formats: ['dmg'], name: 'sample', hdiutil: fakeHdiutil(calls) });
  const target = path.join(root, 'sample.dmg');
  assert.deepEqual([...result.out], [target]);
  assert.equal(await readFile(target, 'utf8'), 'FAKE-DMG');
  assert.equal(calls.length, 1);
  const { args, plist } = calls[0];
  assert.equal(args[0], 'create');
  assert.equal(args[1], '-srcfolder');
  assert.deepEqual(args.slice(3, -1), ['-volname', 'Sample App', '-fs', 'HFS+', '-format', 'UDZO', '-ov']);
  assert.equal(path.basename(args.at(-1)), 'image.dmg');
  assert.equal(plist, renderInfoPlist({ id: 'com.example.app', name: 'Sample App', executable: 'sample' }));
  assert.deepEqual(await leftovers(root), [], 'the work directory is removed');
}));
test('dmg_is_refused_before_any_write_for_other_platforms_and_for_a_host_without_hdiutil', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  const calls = [];
  for (const platform of ['win32-x64', 'linux-x64']) {
    await assert.rejects(bundle(packagePath, { runtime, platform, formats: ['dmg'], name: 'sample', hdiutil: fakeHdiutil(calls) }),
      /Bundle: format dmg is not available for platform/, platform);
  }
  if (process.platform !== 'darwin') {
    await assert.rejects(bundle(packagePath, { runtime, platform: 'darwin-arm64', formats: ['dmg'], name: 'sample' }),
      /Bundle: format dmg can be built only on macOS/);
  }
  await assert.rejects(bundle(packagePath, { runtime, platform: 'darwin-arm64', formats: ['dmg'], name: 'sample', hdiutil: 'hdiutil' }),
    /Bundle: hdiutil must be a function or undefined/);
  assert.equal(calls.length, 0);
  assert.deepEqual((await readdir(root)).sort(), ['app', 'home', 'pkg.alef', 'runtime'], 'nothing was written');
}));
test('dmg_failures_of_the_tool_leave_no_output_and_an_existing_image_needs_force', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  const base = { runtime, platform: 'darwin-arm64', formats: ['dmg'], name: 'sample' };
  await assert.rejects(bundle(packagePath, { ...base, hdiutil: async () => { throw new Error('hdiutil: no space left'); } }), /no space left/);
  await assert.rejects(bundle(packagePath, { ...base, hdiutil: async () => {} }), /Bundle: hdiutil did not produce the disk image/);
  assert.deepEqual((await readdir(root)).sort(), ['app', 'home', 'pkg.alef', 'runtime'], 'no image and no work directory remain');
  await writeFile(path.join(root, 'sample.dmg'), 'OLD');
  const calls = [];
  await assert.rejects(bundle(packagePath, { ...base, hdiutil: fakeHdiutil(calls) }), /already exists \(use --force\)/);
  assert.equal(calls.length, 0, 'the tool is not run for a refused target');
  assert.equal(await readFile(path.join(root, 'sample.dmg'), 'utf8'), 'OLD');
  await bundle(packagePath, { ...base, force: true, hdiutil: fakeHdiutil(calls) });
  assert.equal(await readFile(path.join(root, 'sample.dmg'), 'utf8'), 'FAKE-DMG');
  assert.deepEqual(await leftovers(root), []);
}));
test('dmg_volume_name_drops_separators_and_is_cut_to_27_characters', bundleBounded, () => bundleScratch(async root => {
  const volumes = [];
  const tool = async args => { volumes.push(args[args.indexOf('-volname') + 1]); await writeFile(args.at(-1), 'FAKE-DMG'); };
  for (const [index, name] of ['A/B:C', 'x'.repeat(40)].entries()) {
    const sub = path.join(root, `n${index}`);
    await mkdir(sub);
    const { packagePath, runtime } = await makeFixture(sub, { name });
    await bundle(packagePath, { runtime, platform: 'darwin-x64', formats: ['dmg'], name: 'sample', hdiutil: tool });
  }
  assert.deepEqual(volumes, ['A_B_C', 'x'.repeat(27)]);
}));
// The real tool on a macOS runner: CI turns it on (ALEF_TEST_HDIUTIL=1); the app inside is never launched.
test('hdiutil_builds_a_verifiable_mountable_dmg_with_the_app_bundle', {
  timeout: 300000,
  skip: process.env.ALEF_TEST_HDIUTIL === '1' ? false : 'ALEF_TEST_HDIUTIL=1 is not set',
}, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  const platform = process.arch === 'arm64' ? 'darwin-arm64' : 'darwin-x64';
  await bundle(packagePath, { runtime, platform, formats: ['dmg'], name: 'sample' });
  const image = path.join(root, 'sample.dmg');
  const run = promisify(execFile);
  await run('hdiutil', ['verify', image]);
  const mount = path.join(root, 'mount');
  await mkdir(mount);
  await run('hdiutil', ['attach', '-nobrowse', '-readonly', '-noverify', '-mountpoint', mount, image]);
  try {
    assert.equal(await readFile(path.join(mount, 'sample.app', 'Contents', 'Info.plist'), 'utf8'),
      renderInfoPlist({ id: 'com.example.app', name: 'Sample App', executable: 'sample' }));
    assert.ok((await lstat(path.join(mount, 'sample.app', 'Contents', 'Resources', 'alef'))).isFile());
  } finally {
    await run('hdiutil', ['detach', mount, '-force']);
  }
}));
test('app_bundle_matches_renderinfoplist_exactly_and_the_app_zip_prefixes_entries_with_the_app_tree', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  const result = await bundle(packagePath, { runtime, platform: 'darwin-arm64', formats: ['app'], name: 'sample' });
  const appDir = path.join(root, 'sample.app');
  assert.deepEqual([...result.out], [appDir]);
  const plist = await readFile(path.join(appDir, 'Contents', 'Info.plist'), 'utf8');
  assert.equal(plist, renderInfoPlist({ id: 'com.example.app', name: 'Sample App', executable: 'sample' }));
  const launcher = path.join(appDir, 'Contents', 'MacOS', 'sample');
  assert.equal(await readFile(launcher, 'utf8'), appLauncher);
  const launcherMode = await posixMode(launcher);
  if (launcherMode !== undefined) assert.equal(launcherMode, 0o755);
  assert.equal(fileHash(await readFile(path.join(appDir, 'Contents', 'Resources', 'app.alef'))), fileHash(await readFile(packagePath)));
  const zipped = await bundle(packagePath, { runtime, platform: 'darwin-arm64', formats: ['app', 'zip'], name: 'sample2' });
  const zipPath = path.join(root, 'sample2.app.zip');
  assert.ok(zipped.out.includes(appDir.replace('sample.app', 'sample2.app')) && zipped.out.includes(zipPath));
  const archive = await readZip(await readFile(zipPath));
  assert.ok(archive.entries.length > 0);
  assert.ok(archive.entries.every(entry => entry.name.startsWith('sample2.app/')));
  assert.ok(archive.entries.some(entry => entry.name === 'sample2.app/Contents/MacOS/sample2'));
  assert.equal(parseZipCentral(await readFile(zipPath)).find(entry => entry.name === 'sample2.app/Contents/MacOS/sample2').mode, 0o100755);
}));
test('icon_png_is_carried_into_the_deb_data_tar_and_the_app_resources_with_a_plist_reference', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root, { icon: true });
  const deb = await bundle(packagePath, { runtime, platform: 'linux-x64', formats: ['deb'], name: 'sample' });
  const members = parseAr(await readFile(deb.out[0]));
  const dataTar = zlib.gunzipSync(members.find(member => member.name === 'data.tar.gz').data);
  const entries = parseUstar(dataTar);
  const iconEntry = entries.find(entry => entry.name === './usr/share/icons/hicolor/256x256/apps/sample-app.png');
  assert.ok(iconEntry !== undefined && iconEntry.size === 11);
  const desktopEntry = entries.find(entry => entry.name === './usr/share/applications/sample-app.desktop');
  const desktopAt = dataTar.indexOf(Buffer.from('[Desktop Entry]'));
  const desktop = dataTar.subarray(desktopAt, desktopAt + 512).toString('utf8').split('\0')[0];
  assert.ok(desktop.includes('Icon=sample-app'), desktop);
  assert.ok(desktopEntry !== undefined);
  await bundle(packagePath, { runtime, platform: 'darwin-arm64', formats: ['app'], name: 'sample' });
  const appDir = path.join(root, 'sample.app');
  assert.deepEqual(await readFile(path.join(appDir, 'Contents', 'Resources', 'icon.png')), Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3]));
  const plist = await readFile(path.join(appDir, 'Contents', 'Info.plist'), 'utf8');
  assert.ok(plist.includes('<key>CFBundleIconFile</key><string>icon.png</string>'), plist);
}));
test('zip_prefixes_entries_with_a_sanitized_stem_and_tar_gz_keeps_the_raw_default_stem', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  // The zip ENTRY PREFIX is ASCII-sanitized; the artifact file name keeps the stem.
  const zipped = await bundle(packagePath, { runtime, platform: 'win32-x64', formats: ['zip'] });
  assert.equal(zipped.stem, 'Sample App-1.2.3-win32-x64');
  const zipPath = path.join(root, 'Sample App-1.2.3-win32-x64.zip');
  assert.ok((await lstat(zipPath)).isFile());
  const archive = await readZip(await readFile(zipPath));
  assert.ok(archive.entries.every(entry => entry.name.startsWith('Sample_App-1.2.3-win32-x64/')), archive.entries.map(e => e.name));
  const tar = await bundle(packagePath, { runtime, platform: 'win32-x64', formats: ['tar.gz'] });
  assert.equal(tar.stem, 'Sample App-1.2.3-win32-x64');
  assert.ok((await lstat(path.join(root, 'Sample App-1.2.3-win32-x64.tar.gz'))).isFile());
  await assert.rejects(bundle(packagePath, { runtime, platform: 'win32-x64', formats: ['nsis'] }),
    error => error.message === 'Bundle: Sample App-1.2.3-win32-x64: the nsis format requires an ASCII stem (letters, digits, dot, underscore, dash); pass name');
}));
// Shared barrier so two concurrent bundle() calls both pass preflight, then
// publish together: the exclusive publication alone decides the winner.
function raceGate() {
  let arrived = 0; let release;
  const gate = new Promise(resolve => { release = resolve; });
  const afterPreflight = async () => { if (++arrived === 2) release(); await gate; };
  return { afterPreflight, arrivals: () => arrived };
}
test('concurrent_file_bundles_race_on_the_same_target_and_the_winner_is_byte_identical', bundleBounded, () => bundleScratch(async root => {
  const sharedHome = path.join(root, 'home');
  await mkdir(sharedHome, { recursive: true }); await createKey(undefined, { home: sharedHome });
  const fixtureBytes = Buffer.from(Array.from({ length: 4096 }, (_, index) => (index * 31 + 7) & 255));
  const { packagePath, runtime } = await makeFixture(root, { runtimeData: fixtureBytes, home: sharedHome });
  const gate = raceGate();
  const options = { runtime, platform: 'linux-x64', formats: ['tar.gz'], name: 'arch', afterPreflight: gate.afterPreflight };
  const settled = await Promise.allSettled([bundle(packagePath, options), bundle(packagePath, options)]);
  assert.equal(gate.arrivals(), 2, 'both runs must reach the barrier');
  const fulfilled = settled.filter(result => result.status === 'fulfilled');
  const rejected = settled.filter(result => result.status === 'rejected');
  assert.equal(fulfilled.length, 1, JSON.stringify(settled.map(result => result.status)));
  assert.equal(rejected.length, 1);
  assert.ok(rejected[0].reason.message.endsWith('already exists (use --force)'), rejected[0].reason.message);
  // Determinism oracle: a fresh single run of the same fixture, same key home.
  const oracle = await bundleScratch(async other => {
    const fresh = await makeFixture(other, { runtimeData: fixtureBytes, home: sharedHome });
    const result = await bundle(fresh.packagePath, { runtime: fresh.runtime, platform: 'linux-x64', formats: ['tar.gz'], name: 'arch' });
    return readFile(result.out[0]);
  });
  const winner = await readFile(fulfilled[0].value.out[0]);
  assert.deepEqual(winner, oracle);
}));
test('concurrent_dir_bundles_race_on_the_same_target_and_the_survivor_is_uncorrupted', bundleBounded, () => bundleScratch(async root => {
  const runtimeBytes = randomBytes(512);
  const { packagePath, runtime } = await makeFixture(root, { runtimeData: runtimeBytes });
  const gate = raceGate();
  const options = { runtime, platform: 'linux-x64', formats: ['dir'], name: 'sample', afterPreflight: gate.afterPreflight };
  const settled = await Promise.allSettled([bundle(packagePath, options), bundle(packagePath, options)]);
  assert.equal(gate.arrivals(), 2, 'both runs must reach the barrier');
  const fulfilled = settled.filter(result => result.status === 'fulfilled');
  const rejected = settled.filter(result => result.status === 'rejected');
  assert.equal(fulfilled.length, 1, JSON.stringify(settled.map(result => result.status)));
  assert.equal(rejected.length, 1);
  assert.ok(rejected[0].reason.message.endsWith('already exists (use --force)'), rejected[0].reason.message);
  const layout = path.join(root, 'sample');
  assert.deepEqual(await readFile(path.join(layout, 'bin', 'alef')), runtimeBytes);
  assert.deepEqual(await readFile(path.join(layout, 'app', 'app.alef')), await readFile(packagePath));
  const forced = await bundle(packagePath, { runtime, platform: 'linux-x64', formats: ['dir'], name: 'sample', force: true });
  assert.deepEqual([...forced.out], [layout]);
}));
test('force_reaches_every_publication_and_forced_artifacts_are_byte_identical', { timeout: 120000 }, () => bundleScratch(async root => {
  const sharedHome = path.join(root, 'home');
  await mkdir(sharedHome, { recursive: true }); await createKey(undefined, { home: sharedHome });
  const cases = [
    ['dir', 'win32-x64', 'sample'], ['zip', 'win32-x64', 'sample.zip'],
    ['tar.gz', 'linux-x64', 'sample.tar.gz'], ['deb', 'linux-x64', 'sample.deb'],
    ['nsis', 'win32-x64', 'sample-nsis'], ['app', 'darwin-arm64', 'sample.app'],
  ];
  const fixtureBytes = Buffer.from(Array.from({ length: 1024 }, (_, index) => (index * 13 + 5) & 255));
  for (const [format, platform, artifactName] of cases) {
    const sub = path.join(root, format.replaceAll('.', '_'));
    await mkdir(sub, { recursive: true });
    const { packagePath, runtime } = await makeFixture(sub, { runtimeData: fixtureBytes, home: sharedHome });
    const options = { runtime, platform, formats: [format], name: 'sample' };
    await bundle(packagePath, options);
    const target = path.join(sub, artifactName);
    await assert.rejects(bundle(packagePath, options),
      error => error.message === `Bundle: ${target} already exists (use --force)`);
    await bundle(packagePath, { ...options, force: true });
    assert.ok((await lstat(target)).isFile() || (await lstat(target)).isDirectory(), target);
    if (format === 'app') {
      assert.deepEqual(await readFile(path.join(target, 'Contents', 'Resources', 'app.alef')), await readFile(packagePath));
    } else if (format === 'dir' || format === 'nsis') {
      assert.deepEqual(await readFile(path.join(target, 'app', 'app.alef')), await readFile(packagePath));
    } else {
      // Determinism oracle: a fresh single run with the same fixture and key home.
      const oracle = await bundleScratch(async other => {
        const fresh = await makeFixture(other, { runtimeData: fixtureBytes, home: sharedHome });
        const result = await bundle(fresh.packagePath, { runtime: fresh.runtime, platform, formats: [format], name: 'sample' });
        return readFile(result.out[0]);
      });
      assert.deepEqual(await readFile(target), oracle, format);
    }
  }
}));
test('zip_with_a_spaced_default_stem_sanitizes_every_entry_name', bundleBounded, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  await bundle(packagePath, { runtime, platform: 'darwin-arm64', formats: ['app', 'zip'] });
  const zipStem = 'Sample_App-1.2.3-darwin-arm64';
  const zipPath = path.join(root, 'Sample App-1.2.3-darwin-arm64.app.zip');
  assert.ok((await lstat(zipPath)).isFile());
  const raw = await readFile(zipPath);
  const archive = await readZip(raw);
  const names = archive.entries.map(entry => entry.name);
  // Every entry name byte is in the ASCII zip charset.
  assert.ok(names.every(name => [...name].every(char => /[A-Za-z0-9._@+/-]/.test(char))), names.join(','));
  const launcher = `${zipStem}.app/Contents/MacOS/${zipStem}`;
  assert.ok(names.includes(launcher), names.join(','));
  assert.equal(parseZipCentral(raw).find(entry => entry.name === launcher).mode, 0o100755);
  // The plist executable must agree with the MacOS launcher entry name.
  const plist = (await archive.read(`${zipStem}.app/Contents/Info.plist`)).toString('utf8');
  const executable = plist.match(/<key>CFBundleExecutable<\/key><string>([^<]+)<\/string>/);
  assert.ok(executable !== null, plist);
  assert.equal(executable[1], path.posix.basename(launcher));
  assert.ok(plist.includes('<key>CFBundleIdentifier</key><string>com.example.app</string>'), plist);
}));
test('dir_launcher_resolves_through_a_symlink_to_the_real_layout', { skip: process.platform === 'win32', timeout: 30000 }, () => bundleScratch(async root => {
  const runtimeBytes = Buffer.from('#!/bin/sh\necho "alef-args:$*"\n', 'utf8');
  const { packagePath, runtime } = await makeFixture(root, { runtimeData: runtimeBytes });
  // Bundle into a fresh output root so the symlink never collides with the
  // fixture's `app` directory (which holds dist/).
  const out = path.join(root, 'bundles');
  await mkdir(out);
  await bundle(packagePath, { runtime, platform: 'linux-x64', formats: ['dir'], name: 'tool', out });
  // The symlink lives in a DIFFERENT directory than the real launcher: without
  // readlink resolution DIR would be <root>/bin and the launcher would fail.
  await mkdir(path.join(root, 'bin'));
  await symlink(path.join(out, 'tool', 'tool'), path.join(root, 'bin', 'link'));
  const run = promisify(execFile);
  const { stdout } = await run('/bin/sh', [path.join(root, 'bin', 'link')], { timeout: 10000 });
  assert.equal(stdout.startsWith('alef-args:run '), true, stdout);
  assert.ok(stdout.includes('bundles/tool/app/app.alef'), stdout);
  assert.ok(!stdout.includes('/bin/bin/'), stdout);
}));
test('symlinked_path_components_of_the_runtime_are_refused', { skip: process.platform === 'win32', timeout: 30000 }, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  const realDir = path.join(root, 'realDir');
  await mkdir(realDir);
  await rename(runtime, path.join(realDir, 'runtime'));
  // The scratch root is under tmpdir, so this symlink is NOT exempt from the walk.
  await symlink(realDir, path.join(root, 'sym'));
  await assert.rejects(
    bundle(packagePath, { runtime: path.join(root, 'sym', 'runtime'), platform: 'linux-x64', formats: ['dir'], name: 'sample' }),
    error => error.message.includes('Symlink forbidden'),
  );
}));

test('letters_x_inside_prerelease_and_build_identifiers_are_not_wildcards', () => {
  assert.equal(satisfiesRange('1.2.3', '=1.2.3+exp'), true);
  assert.equal(satisfiesRange('1.2.4', '>=1.2.3-next.1'), true);
  assert.throws(() => parseRuntimeRange('>=*'), /wildcard/);
  assert.throws(() => parseRuntimeRange('^x'), /wildcard/);
});

test('a_name_that_collides_with_a_fixed_layout_entry_is_refused', { skip: process.platform === 'win32', timeout: 30000 }, () => bundleScratch(async root => {
  const { packagePath, runtime } = await makeFixture(root);
  for (const name of ['app', 'bin', 'bundle.json', 'LICENSE']) {
    await assert.rejects(
      bundle(packagePath, { runtime, platform: 'linux-x64', formats: ['dir'], name, out: path.join(root, 'o-' + name.replace('.', '_')) }),
      error => error.message.includes('collides with a fixed entry'),
      name,
    );
  }
}));
