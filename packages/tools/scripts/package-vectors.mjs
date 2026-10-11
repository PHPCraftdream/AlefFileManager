// SPDX-License-Identifier: MIT OR Apache-2.0
// Writes tests/fixtures/package-vectors.txt: archives of format 1 built here, from the text of the
// format only (not with the writer of the packer, not with the reader of the crate), one per line:
//   <name> TAB <signed:<fingerprint>|unsigned|reject> TAB <archive in hex>
// Both readers must give the same answer for every line. Usage: node packages/tools/scripts/package-vectors.mjs
import { createHash, createPrivateKey, createPublicKey, sign } from 'node:crypto';
import { crc32, deflateRawSync } from 'node:zlib';
import { writeFileSync } from 'node:fs';

const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const u16 = value => { const b = Buffer.alloc(2); b.writeUInt16LE(value); return b; };
const u32 = value => { const b = Buffer.alloc(4); b.writeUInt32LE(value >>> 0); return b; };
const text = (...parts) => Buffer.from(parts.join(''), 'latin1');

function keyFrom(label) {
  const seed = createHash('sha256').update(label).digest();
  const priv = createPrivateKey({ key: Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), seed]), format: 'der', type: 'pkcs8' });
  const pub = createPublicKey(priv).export({ format: 'der', type: 'spki' }).subarray(-32);
  return { priv, pub, fingerprint: sha(pub) };
}
const K1 = keyFrom('alef-package-vectors-1');
const K2 = keyFrom('alef-package-vectors-2');
const GROUP_ORDER = 0x1000000000000000000000000000000014def9dea2f79cd65812631a5cf5d3edn;

// Records -> archive bytes. Every field a reader must check can be overridden.
function build(items, options = {}) {
  const prefix = options.prefix ?? Buffer.alloc(0);
  const chunks = [prefix]; let at = prefix.length; const central = [];
  for (const r of items) {
    const name = Buffer.isBuffer(r.name) ? r.name : Buffer.from(r.name, 'latin1');
    const method = r.method ?? 0;
    const body = r.body ?? (method === 8 ? deflateRawSync(r.data, { level: 9 }) : r.data);
    const crc = r.crc ?? crc32(r.data);
    const flags = r.flags ?? 0x0800;
    const local = Buffer.concat([
      text('PK\x03\x04'), u16(r.lver ?? 20), u16(r.lflags ?? flags), u16(r.lmethod ?? method), u16(r.ltime ?? 0), u16(r.ldate ?? 0x21),
      u32(r.lcrc ?? crc), u32(r.lcomp ?? body.length), u32(r.lsize ?? r.data.length), u16((r.lname ?? name).length), u16((r.lextra ?? Buffer.alloc(0)).length),
      r.lname ?? name, r.lextra ?? Buffer.alloc(0), body, r.descriptor ?? Buffer.alloc(0), r.gapAfter ?? Buffer.alloc(0),
    ]);
    if (!r.hidden) central.push({ r, name, method, body, crc, flags, offset: r.coffset ?? at });
    chunks.push(local); at += local.length;
  }
  const order = options.centralOrder ?? central.map((_, index) => index);
  const directory = Buffer.concat(order.map(index => {
    const { r, name, method, body, crc, flags, offset } = central[index];
    return Buffer.concat([
      text('PK\x01\x02'), u16(0x031e), u16(r.cver ?? 20), u16(flags), u16(r.cmethod ?? method), u16(r.ctime ?? 0), u16(r.cdate ?? 0x21),
      u32(crc), u32(r.ccomp ?? body.length), u32(r.csize ?? r.data.length), u16(name.length), u16((r.cextra ?? Buffer.alloc(0)).length), u16((r.ccomment ?? Buffer.alloc(0)).length),
      u16(0), u16(0), u32(0x81a40000), u32(offset), name, r.cextra ?? Buffer.alloc(0), r.ccomment ?? Buffer.alloc(0),
    ]);
  }));
  const comment = options.comment ?? Buffer.alloc(0);
  const count = options.count ?? order.length;
  const end = Buffer.concat([text('PK\x05\x06'), u16(0), u16(0), u16(count), u16(count), u32(directory.length), u32(options.cdOffset ?? at), u16(comment.length), comment]);
  return Buffer.concat([...chunks, directory, end, options.suffix ?? Buffer.alloc(0)]);
}

const base64 = bytes => Buffer.from(bytes).toString('base64');
function metadata(files, { key = K1, hashesText, signatureText, signed = true, hashes = true } = {}) {
  const sorted = [...files].sort((a, b) => Buffer.compare(Buffer.from(a[0]), Buffer.from(b[0])));
  const list = hashesText ?? sorted.map(([name, data]) => `${sha(data)}  ${name}\n`).join('');
  const listBytes = Buffer.from(list);
  const signature = signatureText ?? `alef-package 1\nkey ${base64(key.pub)}\nsig ${base64(sign(null, Buffer.concat([Buffer.from('alef-package-v1\n'), listBytes]), key.priv))}\n`;
  const out = [];
  if (hashes) out.push({ name: 'META-INF/hashes', data: listBytes });
  if (signed) out.push({ name: 'META-INF/signature', data: Buffer.from(signature) });
  return out;
}
const FILES = [['alef.ktav', Buffer.from('id: org.example.demo\n')], ['dist/index.html', Buffer.from('<!doctype html>\n'.repeat(40))], ['icon.png', Buffer.from([0x89, 0x50, 0x4e, 0x47, 1, 2, 3, 4])]];
function records(files = FILES, meta = {}, fix = () => {}) {
  const all = [...files.map(([name, data]) => ({ name, data })), ...metadata(files, meta)];
  all.sort((a, b) => Buffer.compare(Buffer.from(a.name), Buffer.from(b.name)));
  fix(all);
  return all;
}
const named = (list, name) => list.find(r => (Buffer.isBuffer(r.name) ? r.name.toString('latin1') : r.name) === name);

const vectors = [];
const add = (name, expect, bytes) => vectors.push([name, expect, bytes]);
const signedBy = key => `signed:${key.fingerprint}`;
const good = () => build(records());

// Valid.
add('valid-signed', signedBy(K1), good());
add('valid-signed-other-key', signedBy(K2), build(records(FILES, { key: K2 })));
add('valid-unsigned-without-hashes', 'unsigned', build(records(FILES, { signed: false, hashes: false })));
add('valid-unsigned-with-hashes', 'unsigned', build(records(FILES, { signed: false })));
add('valid-deflated-entry', signedBy(K1), build(records(FILES, {}, all => { named(all, 'dist/index.html').method = 8; })));
add('valid-empty-file', signedBy(K1), build(records([...FILES, ['dist/empty.txt', Buffer.alloc(0)]])));
add('valid-names-of-the-alphabet', signedBy(K1), build(records([...FILES, ['dist/A-b_c.d@2x+1.js', Buffer.from('x')], ['dist/COM10.txt', Buffer.from('x')], ['dist/CONSOLE.txt', Buffer.from('x')]])));

// The container.
add('zip-prefix-bytes', 'reject', build(records(), { prefix: Buffer.from('MZ-stub') }));
add('zip-suffix-bytes', 'reject', build(records(), { suffix: Buffer.from('tail') }));
add('zip-comment-with-eocd-magic', 'reject', build(records(), { comment: Buffer.from('PK\x05\x06xxxxxxxxxxxxxxxxxx', 'latin1') }));
add('zip-two-archives', 'reject', Buffer.concat([good(), good()]));
add('zip-local-name-differs', 'reject', build(records(FILES, {}, all => { named(all, 'dist/index.html').lname = Buffer.from('dist/other.html'); })));
add('zip-local-name-case-differs', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').lname = Buffer.from('icon.PNG'); })));
add('zip-local-record-not-in-directory', 'reject', build([...records(), { name: 'dist/hidden.js', data: Buffer.from('evil'), hidden: true }]));
add('zip-gap-between-records', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').gapAfter = Buffer.from('gap'); })));
add('zip-overlapping-offsets', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').coffset = 0; })));
add('zip-central-order-differs', 'reject', build(records(), { centralOrder: [1, 0, 2, 3, 4] }));
add('zip-data-descriptor', 'reject', build(records(FILES, {}, all => { const r = named(all, 'icon.png'); r.flags = 0x0808; r.descriptor = Buffer.concat([u32(0x08074b50), u32(crc32(r.data)), u32(r.data.length), u32(r.data.length)]); })));
add('zip-local-extra-field', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').lextra = Buffer.from([0xca, 0xfe, 0, 0]); })));
add('zip-central-extra-field', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').cextra = Buffer.from([0xca, 0xfe, 0, 0]); })));
add('zip-central-entry-comment', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').ccomment = Buffer.from('c'); })));
add('zip-zip64-size-sentinel', 'reject', build(records(FILES, {}, all => { const r = named(all, 'icon.png'); r.csize = 0xffffffff; r.lsize = 0xffffffff; })));
add('zip-zip64-entry-count-sentinel', 'reject', build(records(), { count: 0xffff }));
add('zip-directory-entry', 'reject', build(records(FILES, {}, all => { all.push({ name: 'dist/', data: Buffer.alloc(0) }); all.sort((a, b) => Buffer.compare(Buffer.from(a.name), Buffer.from(b.name))); })));
add('zip-flags-zero', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').flags = 0; })));
add('zip-flags-only-local-differ', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').lflags = 0x0801; })));
add('zip-method-9', 'reject', build(records(FILES, {}, all => { const r = named(all, 'icon.png'); r.method = 9; })));
add('zip-method-differs', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').lmethod = 8; })));
add('zip-local-time-differs', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').ltime = 1; })));
add('zip-local-date-differs', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').ldate = 0x22; })));
add('zip-local-version-differs', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').lver = 10; })));
add('zip-required-version-above-20', 'reject', build(records(FILES, {}, all => { const r = named(all, 'icon.png'); r.lver = 45; r.cver = 45; })));
add('zip-crc-wrong-in-both', 'reject', build(records(FILES, {}, all => { const r = named(all, 'icon.png'); r.crc = 1; r.lcrc = 1; })));
add('zip-crc-differs-local', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').lcrc = 1; })));
add('zip-size-lies-smaller', 'reject', build(records(FILES, {}, all => { const r = named(all, 'dist/index.html'); r.method = 8; r.lsize = 10; r.csize = 10; })));
add('zip-size-lies-larger', 'reject', build(records(FILES, {}, all => { const r = named(all, 'dist/index.html'); r.method = 8; r.lsize = r.data.length + 5; r.csize = r.data.length + 5; })));
add('zip-deflate-trailing-bytes', 'reject', build(records(FILES, {}, all => { const r = named(all, 'dist/index.html'); r.method = 8; r.body = Buffer.concat([deflateRawSync(r.data, { level: 9 }), Buffer.from([1, 2, 3])]); })));
add('zip-deflate-truncated', 'reject', build(records(FILES, {}, all => { const r = named(all, 'dist/index.html'); r.method = 8; r.body = deflateRawSync(r.data, { level: 9 }).subarray(0, 5); })));
add('zip-stored-sizes-differ', 'reject', build(records(FILES, {}, all => { const r = named(all, 'icon.png'); r.lcomp = r.data.length + 1; r.ccomp = r.data.length + 1; r.body = Buffer.concat([r.data, Buffer.from([0])]); })));
add('zip-empty-file', 'reject', Buffer.alloc(0));
add('zip-only-eocd', 'reject', build([]));

// Names.
const withName = (original, replacement, data = Buffer.from('x')) => build(records([...FILES, [replacement, data]]));
add('name-duplicate-differing-in-case', 'reject', withName('', 'Dist/index.html'));
add('name-file-is-prefix-of-path', 'reject', build(records([...FILES, ['dist', Buffer.from('x')]])));
add('name-with-space', 'reject', withName('', 'dist/a b.js'));
add('name-with-non-ascii', 'reject', build(records([...FILES, [Buffer.from('dist/caf\xc3\xa9.js', 'latin1'), Buffer.from('x')]].map(([n, d]) => [Buffer.isBuffer(n) ? n.toString('latin1') : n, d]))));
add('name-windows-device', 'reject', withName('', 'dist/COM1.txt'));
add('name-windows-device-lowercase-no-extension', 'reject', withName('', 'dist/nul'));
add('name-segment-ends-in-dot', 'reject', withName('', 'dist/a.'));
add('name-dot-dot-segment', 'reject', withName('', 'dist/../x.js'));
add('name-absolute', 'reject', withName('', '/dist/x.js'));
add('name-backslash', 'reject', withName('', 'dist\\x.js'));
add('name-empty-segment', 'reject', withName('', 'dist//x.js'));
add('name-percent', 'reject', withName('', 'dist/a%2e.js'));
add('name-foreign-meta-inf', 'reject', withName('', 'META-INF/extra'));
add('name-foreign-meta-inf-lowercase', 'reject', withName('', 'meta-inf/extra'));
add('name-manifest-wrong-case', 'reject', build(records([['Alef.ktav', FILES[0][1]], FILES[1], FILES[2]])));
add('name-manifest-missing', 'reject', build(records([FILES[1], FILES[2]])));
add('name-longer-than-255-in-a-segment', 'reject', withName('', `dist/${'a'.repeat(256)}.js`));

// The hash list.
const swap = (mutate, meta = {}) => build(records(FILES, meta, mutate));
add('hashes-of-another-package', 'reject', swap(all => { named(all, 'META-INF/hashes').data = Buffer.from(`${sha(Buffer.from('other'))}  alef.ktav\n`); }));
add('hashes-a-file-changed-after-signing', 'reject', build(records(FILES, {}, all => { named(all, 'icon.png').data = Buffer.from([0x89, 0x50, 0x4e, 0x47, 1, 2, 3, 5]); })));
add('hashes-extra-line', 'reject', build(records(FILES, { hashesText: `${[...FILES].sort((a, b) => Buffer.compare(Buffer.from(a[0]), Buffer.from(b[0]))).map(([n, d]) => `${sha(d)}  ${n}\n`).join('')}${sha(Buffer.from('z'))}  zzz.js\n` })));
add('hashes-missing-line', 'reject', build(records(FILES, { hashesText: `${sha(FILES[0][1])}  alef.ktav\n` })));
add('hashes-unsorted', 'reject', build(records(FILES, { hashesText: [FILES[2], FILES[1], FILES[0]].map(([n, d]) => `${sha(d)}  ${n}\n`).join('') })));
add('hashes-duplicate-line', 'reject', build(records(FILES, { hashesText: [FILES[0], FILES[0], FILES[1], FILES[2]].map(([n, d]) => `${sha(d)}  ${n}\n`).join('') })));
const canonical = [...FILES].sort((a, b) => Buffer.compare(Buffer.from(a[0]), Buffer.from(b[0]))).map(([n, d]) => `${sha(d)}  ${n}\n`).join('');
add('hashes-crlf', 'reject', build(records(FILES, { hashesText: canonical.replace(/\n/g, '\r\n') })));
add('hashes-bom', 'reject', build(records(FILES, { hashesText: `﻿${canonical}` })));
add('hashes-no-final-lf', 'reject', build(records(FILES, { hashesText: canonical.slice(0, -1) })));
add('hashes-blank-line', 'reject', build(records(FILES, { hashesText: `${canonical}\n` })));
add('hashes-uppercase-hex', 'reject', build(records(FILES, { hashesText: canonical.replace(/^[0-9a-f]+/, m => m.toUpperCase()) })));
add('hashes-one-space', 'reject', build(records(FILES, { hashesText: canonical.replace('  ', ' ') })));
add('hashes-empty', 'reject', build(records(FILES, { hashesText: '' })));
add('hashes-unsigned-but-a-hash-is-wrong', 'reject', build(records(FILES, { signed: false, hashesText: canonical.replace(/^[0-9a-f]{4}/, '0000') })));

// The signature.
const good2 = records(FILES);
const goodSignature = named(good2, 'META-INF/signature').data.toString();
const sigLine = goodSignature.split('\n')[2];
const withSignature = signatureText => build(records(FILES, { signatureText }));
add('sig-one-bit-flipped', 'reject', withSignature(goodSignature.replace(/sig (.)/, (m, c) => `sig ${c === 'A' ? 'B' : 'A'}`)));
add('sig-two-lines', 'reject', withSignature(`alef-package 1\nkey ${base64(K1.pub)}\n`));
add('sig-four-lines', 'reject', withSignature(`${goodSignature}extra\n`));
add('sig-crlf', 'reject', withSignature(goodSignature.replace(/\n/g, '\r\n')));
add('sig-no-final-lf', 'reject', withSignature(goodSignature.slice(0, -1)));
add('sig-wrong-version-line', 'reject', withSignature(goodSignature.replace('alef-package 1', 'alef-package 2')));
add('sig-key-base64-without-padding', 'reject', withSignature(goodSignature.replace(/(key [A-Za-z0-9+/]+)=\n/, '$1\n')));
add('sig-key-base64-pad-bits-set', 'reject', (() => {
  // The last data character before the "=" carries 2 unused bits for a 32-byte key: set one of them.
  const keyLine = `key ${base64(K1.pub)}`;
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  const last = keyLine.slice(0, -2).slice(-1); const bumped = alphabet[alphabet.indexOf(last) | 1];
  const altered = keyLine.slice(0, -3) + bumped + '=';
  return withSignature(goodSignature.replace(keyLine, bumped === last ? keyLine.slice(0, -3) + alphabet[alphabet.indexOf(last) ^ 1] + '=' : altered));
})());
add('sig-over-another-message', 'reject', build(records(FILES, { signatureText: `alef-package 1\nkey ${base64(K1.pub)}\nsig ${base64(sign(null, Buffer.from(`alef-package-v0\n${canonical}`), K1.priv))}\n` })));
add('sig-without-the-domain-prefix', 'reject', build(records(FILES, { signatureText: `alef-package 1\nkey ${base64(K1.pub)}\nsig ${base64(sign(null, Buffer.from(canonical), K1.priv))}\n` })));
add('sig-without-the-hash-list', 'reject', build(records(FILES, { hashes: false })));
add('sig-key-of-small-order', 'reject', build(records(FILES, { signatureText: `alef-package 1\nkey ${base64(Buffer.from('0100000000000000000000000000000000000000000000000000000000000000', 'hex'))}\nsig ${base64(Buffer.concat([Buffer.from('0100000000000000000000000000000000000000000000000000000000000000', 'hex'), Buffer.alloc(32)]))}\n` })));
add('sig-s-not-below-the-group-order', 'reject', (() => {
  const raw = sign(null, Buffer.concat([Buffer.from('alef-package-v1\n'), Buffer.from(canonical)]), K1.priv);
  let s = 0n; for (let i = 31; i >= 0; i--) s = (s << 8n) | BigInt(raw[32 + i]);
  const bigger = s + GROUP_ORDER; const out = Buffer.alloc(32); let v = bigger;
  for (let i = 0; i < 32; i++) { out[i] = Number(v & 0xffn); v >>= 8n; }
  return build(records(FILES, { signatureText: `alef-package 1\nkey ${base64(K1.pub)}\nsig ${base64(Buffer.concat([raw.subarray(0, 32), out]))}\n` }));
})());
add('sig-key-not-32-bytes', 'reject', withSignature(goodSignature.replace(/key [^\n]+/, `key ${base64(K1.pub.subarray(0, 31))}`)));

const lines = vectors.map(([name, expect, bytes]) => `${name}\t${expect}\t${bytes.toString('hex')}\n`);
writeFileSync(new URL('../../../tests/fixtures/package-vectors.txt', import.meta.url), lines.join(''));
console.log(`${vectors.length} vectors; signers K1 ${K1.fingerprint} K2 ${K2.fingerprint}; sigLine ${sigLine.length}`);
