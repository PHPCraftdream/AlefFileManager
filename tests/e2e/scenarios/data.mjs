// SPDX-License-Identifier: MIT OR Apache-2.0
// Scenarios of the data modules (docs/stages/m3-data.md, "Приёмка"): `fs` against the real disk, with
// the scope allowed and with a stand-in the user chose; `store` across two runs of one application.
import * as nodeCrypto from 'node:crypto';
import { createCipheriv, createDecipheriv, createHash, createHmac, createPrivateKey, createPublicKey, hkdfSync, pbkdf2Sync, randomBytes, scryptSync } from 'node:crypto';
import { closeSync, createReadStream, existsSync, ftruncateSync, mkdirSync, mkdtempSync, openSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync, writeSync } from 'node:fs';
import os from 'node:os';
import { join } from 'node:path';

import { prepareSite, startApp, verdictOf } from '../lib.mjs';

const FS_CHECKS = [
  'fs-text-and-bytes-round-trip-with-unicode', 'fs-stat-readdir-rename-copy-and-remove',
  'fs-outside-the-scope-is-denied-however-the-path-is-spelled', 'fs-a-link-that-leads-out-of-the-scope-leads-nowhere',
  'fs-a-scratch-file-and-a-picked-file-need-no-scope', 'fs-a-whole-file-is-for-small-files',
  'fs-a-handle-reads-and-writes-pieces', 'fs-a-big-file-is-copied-through-streams-and-arrives-whole',
  'fs-a-big-folder-comes-in-batches', 'fs-watch-tells-create-modify-and-remove',
];

const SQLITE_CHECKS = [
  'sqlite-schema-insert-and-query-with-every-kind-of-value', 'sqlite-a-transaction-commits-and-a-failing-one-rolls-back',
  'sqlite-a-prepared-statement-runs-with-other-parameters', 'sqlite-a-hundred-thousand-rows-pass-in-order-through-iterate',
  'sqlite-the-connection-is-busy-while-iterating-and-free-after', 'sqlite-errors-say-what-failed',
  'sqlite-sql-cannot-reach-another-file-or-load-code', 'sqlite-a-database-closes-and-is-found-again-in-its-file',
];
const SQLITE_SUBSTITUTE_CHECKS = ['sqlite-a-stand-in-keeps-the-database-and-the-folder-stays-empty'];

const CRYPTO_CHECKS = [
  'digests-and-macs-match-the-openssl-of-node', 'a-big-body-is-digested-whole', 'keys-derived-match-the-openssl-of-node',
  'random-bytes-are-random', 'a-message-sealed-by-node-is-opened-and-the-page-seals-for-node',
  'ed25519-signs-as-node-does-and-checks-what-node-signed', 'what-is-asked-wrongly-is-told',
].map(name => `crypto-${name}`);

const WEBCRYPTO_CHECKS = [
  'state-of-the-engine', 'random-values-and-uuid', 'digest-sha-1', 'digest-sha-256', 'digest-sha-384',
  'digest-sha-512', 'hmac-sha-256', 'hmac-sha-512', 'pbkdf2-sha-256', 'hkdf-sha-256', 'aes-gcm-256', 'aes-cbc-256',
  'aes-ctr-256', 'aes-kw-256', 'derive-key-pbkdf2-to-aes-gcm', 'ecdsa-p-256', 'ecdsa-p-384', 'ecdsa-p-521', 'ecdh-p-256',
  'ecdh-p-384', 'ecdsa-p-256-export-import-jwk-and-spki', 'ed25519', 'x25519', 'rsassa-pkcs1-v1_5-2048', 'rsa-pss-2048',
  'rsa-oaep-2048',
].map(name => `webcrypto-${name}`);

const SECRETS_CHECKS = ['secrets-a-secret-comes-back-as-it-went', 'secrets-the-limits-and-the-names-are-told'];
const SECRETS_SUBSTITUTE_CHECKS = ['secrets-a-stand-in-keeps-what-is-written-until-the-run-ends'];
const SECRETS_DENIED_CHECKS = ['secrets-a-denied-right-keeps-every-command-out'];

const STORE_ID = 'org.alef.e2e.modules.store';
const STORE_CHECKS = {
  write: [
    'store-values-of-every-json-kind-come-back', 'store-keys-come-in-order-by-prefix-and-delete',
    'store-areas-are-apart-and-named-with-care', 'store-a-value-is-up-to-what-a-call-carries', 'store-flush-waits-for-the-disk',
  ],
  read: ['store-what-the-first-run-kept-is-there-after-the-restart', 'store-it-goes-on-working-after-the-restart'],
};

/** Where the runtime keeps the data of the application of this id: the folder `$APPDATA` names. */
function appDataOf(id) {
  const home = os.homedir();
  if (process.platform === 'win32') return join(process.env.LOCALAPPDATA ?? join(home, 'AppData', 'Local'), id);
  if (process.platform === 'darwin') return join(home, 'Library', 'Application Support', id);
  return join(process.env.XDG_DATA_HOME ?? join(home, '.local', 'share'), id);
}

const COPY_SIZE = 96 * 1024 * 1024 + 4321;
const MANY = 2500;

/** The sha-256 of a file, read as a stream. */
function sha256(path) {
  return new Promise((resolvePromise, reject) => {
    const hash = createHash('sha256');
    createReadStream(path).on('data', chunk => hash.update(chunk)).on('error', reject).on('end', () => resolvePromise(hash.digest('hex')));
  });
}

const SUBSTITUTE_CHECKS = [
  'fs-a-stand-in-starts-as-an-empty-folder', 'fs-a-stand-in-keeps-what-is-written-and-answers-like-a-disk',
  'fs-a-stand-in-refuses-what-the-scope-refuses',
];

/** Every file below `directory`, as paths relative to it. */
function filesBelow(directory) {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true, recursive: true })
    .filter(entry => entry.isFile())
    .map(entry => join(entry.parentPath ?? entry.path, entry.name).slice(directory.length + 1));
}

/** The folders the page works in: a scope, a folder beside it, a picked file, a link and a big file. */
function prepare() {
  const base = mkdtempSync(join(os.tmpdir(), 'alef-e2e-fs-'));
  const root = join(base, 'root');
  const outside = join(base, 'outside');
  mkdirSync(root);
  mkdirSync(outside);
  writeFileSync(join(outside, 'secret.txt'), 'secret');
  writeFileSync(join(base, 'picked.txt'), 'picked');
  writeFileSync(join(base, 'neighbour.txt'), 'neighbour');
  writeFileSync(join(root, 'real.txt'), 'real');
  mkdirSync(join(root, 'real-folder'));
  let link = null;
  try {
    symlinkSync(join(outside, 'secret.txt'), join(root, 'link'));
    link = join(root, 'link');
  } catch {
    // an account that may not make links: the page says so
  }
  const many = join(root, 'many');
  mkdirSync(many);
  for (let index = 0; index < MANY; index += 1) writeFileSync(join(many, `f${String(index).padStart(5, '0')}.txt`), 'x');
  const copySource = join(root, 'copy-source.bin');
  const source = openSync(copySource, 'w');
  for (let written = 0; written < COPY_SIZE; written += 1024 * 1024) writeSync(source, randomBytes(Math.min(1024 * 1024, COPY_SIZE - written)));
  closeSync(source);
  const big = join(root, 'big.bin');
  const handle = openSync(big, 'w');
  ftruncateSync(handle, 64 * 1024 * 1024 + 1);
  closeSync(handle);
  return {
    base, root, link, big, many, copySource, outside,
    secret: join(outside, 'secret.txt'), picked: join(base, 'picked.txt'),
    scope: `${root.replaceAll('\\', '/')}/**`,
  };
}

const CIPHERS = { 'aes-128-gcm': 16, 'aes-256-gcm': 32, 'chacha20-poly1305': 32 };
const ED25519_PKCS8 = Buffer.from('302e020100300506032b657004220420', 'hex');
const ED25519_SPKI = Buffer.from('302a300506032b6570032100', 'hex');

/** What the page of `crypto` is to find, worked out by the OpenSSL of Node, and the check of what the page sealed. */
function cryptoOracle() {
  const text = 'héllo — мир 🌍';
  const plain = Buffer.from(text);
  const salt = randomBytes(16);
  const key = randomBytes(32);
  const info = randomBytes(10);
  const digest = {};
  const hmac = {};
  for (const [name, node] of [['sha-1', 'sha1'], ['sha-256', 'sha256'], ['sha-384', 'sha384'], ['sha-512', 'sha512']]) {
    digest[name] = createHash(node).update(plain).digest('hex');
    hmac[name] = createHmac(node, key).update(plain).digest('hex');
  }
  const big = Buffer.alloc(8 * 1024 * 1024);
  for (let index = 0; index < big.length; index += 1) big[index] = (index * 31 + (index >> 8)) & 255;
  const sealed = {};
  for (const [cipher, length] of Object.entries(CIPHERS)) {
    const sealKey = randomBytes(length);
    const aad = randomBytes(8);
    const nonce = randomBytes(12);
    const sealer = createCipheriv(cipher, sealKey, nonce, { authTagLength: 16 });
    sealer.setAAD(aad, { plaintextLength: plain.length });
    const body = Buffer.concat([sealer.update(plain), sealer.final()]);
    sealed[cipher] = {
      keyHex: sealKey.toString('hex'), aadHex: aad.toString('hex'),
      messageHex: Buffer.concat([nonce, body, sealer.getAuthTag()]).toString('hex'),
    };
  }
  const seed = randomBytes(32);
  const privateKey = createPrivateKey({ key: Buffer.concat([ED25519_PKCS8, seed]), format: 'der', type: 'pkcs8' });
  const publicKey = createPublicKey(privateKey).export({ format: 'der', type: 'spki' }).subarray(-32);
  const targets = {
    saltHex: salt.toString('hex'), keyHex: key.toString('hex'), infoHex: info.toString('hex'), digest, hmac,
    bigDigest: createHash('sha256').update(big).digest('hex'),
    hkdf: Buffer.from(hkdfSync('sha256', plain, salt, info, 42)).toString('hex'),
    pbkdf2: pbkdf2Sync(plain, salt, 1000, 48, 'sha256').toString('hex'),
    scrypt: scryptSync(plain, salt, 40, { N: 1024, r: 8, p: 1 }).toString('hex'),
    argon2id: typeof nodeCrypto.argon2Sync === 'function'
      ? nodeCrypto.argon2Sync('argon2id', { message: plain, nonce: salt, parallelism: 2, tagLength: 40, memory: 4096, passes: 3 }).toString('hex')
      : null,
    sealed,
    ed25519: {
      seedHex: seed.toString('hex'), publicHex: publicKey.toString('hex'),
      signature: nodeCrypto.sign(null, plain, privateKey).toString('hex'),
    },
  };
  const judge = lines => {
    const problems = [];
    for (const cipher of Object.keys(CIPHERS)) {
      const line = lines.map(item => new RegExp(`ALEF_E2E sealed ${cipher} ([0-9a-f]+)`).exec(item)?.[1]).find(Boolean);
      if (!line) {
        problems.push(`the page did not report what it sealed with ${cipher}`);
        continue;
      }
      try {
        const message = Buffer.from(line, 'hex');
        const opener = createDecipheriv(cipher, Buffer.from(sealed[cipher].keyHex, 'hex'), message.subarray(0, 12), { authTagLength: 16 });
        opener.setAAD(Buffer.from(sealed[cipher].aadHex, 'hex'), { plaintextLength: message.length - 28 });
        opener.setAuthTag(message.subarray(message.length - 16));
        const opened = Buffer.concat([opener.update(message.subarray(12, message.length - 16)), opener.final()]);
        if (!opened.equals(plain)) problems.push(`${cipher}: Node opened another text`);
      } catch (error) {
        problems.push(`${cipher}: Node could not open what the page sealed: ${error.message}`);
      }
    }
    const signed = lines.map(item => /ALEF_E2E ed25519 ([0-9a-f]{64}) ([0-9a-f]{128})/.exec(item)).find(Boolean);
    if (!signed) {
      problems.push('the page did not report its Ed25519 signature');
    } else {
      const generated = createPublicKey({ key: Buffer.concat([ED25519_SPKI, Buffer.from(signed[1], 'hex')]), format: 'der', type: 'spki' });
      if (!nodeCrypto.verify(null, plain, generated, Buffer.from(signed[2], 'hex'))) problems.push('Node does not verify the signature of the page');
    }
    return problems;
  };
  return { targets, judge };
}

export function dataScenarios({ drive, exe, verbose }) {
  return {
    // The user allowed the scope: everything the page does is real and stays inside it.
    async fs() {
      const here = prepare();
      try {
        return await drive({
          name: 'fs', app: 'modules/data/fs', replacements: { ROOT: here.scope.slice(0, -3) },
          targets: {
            mode: 'real', root: here.root, outside: here.secret, outsideName: 'outside', outsideFolder: here.outside, link: here.link,
            big: here.big, many: here.many, manyCount: MANY, copySource: here.copySource, copySize: COPY_SIZE,
          },
          env: { ALEF_HOME: join(here.base, 'home'), ALEF_E2E_DIALOGS: JSON.stringify([{ open: [here.picked] }]) },
          expectedChecks: FS_CHECKS,
          judge: async () => {
            const problems = [];
            const copied = join(here.root, 'work', 'copied.bin');
            if (!existsSync(copied) || await sha256(copied) !== await sha256(here.copySource)) problems.push('the copy made through streams is not the file');
            const note = join(here.root, 'work', 'note.txt');
            if (!existsSync(note) || readFileSync(note, 'utf8') !== 'héllo — мир 🌍!') problems.push('the note is not on the disk as the page wrote it');
            if (readFileSync(here.secret, 'utf8') !== 'secret') problems.push('the file outside the scope was changed');
            if (readFileSync(here.picked, 'utf8') !== 'picked') problems.push('the picked file was changed');
            if (here.link && existsSync(here.link)) problems.push('the link is still there');
            if (here.link && !existsSync(here.secret)) problems.push('removing the link removed what it led to');
            if (!existsSync(join(here.root, 'work', 'x2', 'y', 'copy.txt'))) problems.push('the copied folder is missing');
            return problems;
          },
        });
      } finally {
        rmSync(here.base, { recursive: true, force: true });
      }
    },

    // The user chose a stand-in for the scope: the page finds an empty folder and keeps its writes in it;
    // the real folder is as it was, and what the page wrote lies in the folder of the runtime.
    async 'fs-substitute'() {
      const here = prepare();
      const home = join(here.base, 'home');
      try {
        const before = filesBelow(here.root).sort();
        const result = await drive({
          name: 'fs-substitute', app: 'modules/data/fs', replacements: { ROOT: here.scope.slice(0, -3) },
          targets: { mode: 'substituted', root: here.root, outside: here.secret, outsideName: 'outside' },
          env: {
            ALEF_HOME: home,
            ALEF_E2E_CONSENT: `fs.read:${here.scope}=substitute;fs.write:${here.scope}=substitute`,
          },
          expectedChecks: SUBSTITUTE_CHECKS,
          judge: () => {
            const problems = [];
            const after = filesBelow(here.root).sort();
            if (JSON.stringify(after) !== JSON.stringify(before)) problems.push(`the real folder changed: ${JSON.stringify(before)} -> ${JSON.stringify(after)}`);
            const kept = filesBelow(join(home, 'shadow'));
            for (const name of ['renamed.txt', 'copied.txt']) {
              if (!kept.some(path => path.endsWith(name))) problems.push(`${name} is not in the folder of the stand-in`);
            }
            if (!kept.some(path => path.endsWith(join('a', 'b', 'deep.txt')))) problems.push('the nested file is not in the folder of the stand-in');
            return problems;
          },
        });
        return result;
      } finally {
        rmSync(here.base, { recursive: true, force: true });
      }
    },

    // A database in the scope the user allowed: the page works with it, and the runner opens the file itself.
    async sqlite() {
      const base = mkdtempSync(join(os.tmpdir(), 'alef-e2e-sqlite-'));
      const root = join(base, 'root');
      const outside = join(base, 'outside');
      mkdirSync(root);
      mkdirSync(outside);
      try {
        return await drive({
          name: 'sqlite', app: 'modules/data/sqlite', replacements: { ROOT: root.replaceAll('\\', '/') },
          targets: { mode: 'real', root, outside, many: 100000 },
          env: { ALEF_HOME: join(base, 'home') },
          expectedChecks: SQLITE_CHECKS,
          judge: async () => {
            const problems = [];
            const file = join(root, 'app.db');
            if (!existsSync(file)) return ['the database is not in the folder of the scope'];
            if (filesBelow(outside).length !== 0) problems.push(`something was made outside the scope: ${filesBelow(outside)}`);
            try {
              const { DatabaseSync } = await import('node:sqlite');
              const database = new DatabaseSync(file, { readOnly: true });
              const count = database.prepare('SELECT count(*) AS n FROM items').get().n;
              if (count !== 24) problems.push(`the file holds ${count} items, expected 24`);
              const check = database.prepare('PRAGMA integrity_check').get().integrity_check;
              if (check !== 'ok') problems.push(`integrity_check: ${check}`);
              database.close();
            } catch (error) {
              if (error?.code !== 'ERR_UNKNOWN_BUILTIN_MODULE') problems.push(`the runner could not read the file: ${error.message}`);
            }
            return problems;
          },
        });
      } finally {
        rmSync(base, { recursive: true, force: true });
      }
    },

    // The user chose a stand-in: the database is in the folder of the runtime, the real folder stays empty.
    async 'sqlite-substitute'() {
      const base = mkdtempSync(join(os.tmpdir(), 'alef-e2e-sqlite-'));
      const root = join(base, 'root');
      const outside = join(base, 'outside');
      mkdirSync(root);
      mkdirSync(outside);
      const home = join(base, 'home');
      const scope = `${root.replaceAll('\\', '/')}/**`;
      try {
        return await drive({
          name: 'sqlite-substitute', app: 'modules/data/sqlite', replacements: { ROOT: root.replaceAll('\\', '/') },
          targets: { mode: 'substituted', root, outside },
          env: { ALEF_HOME: home, ALEF_E2E_CONSENT: `fs.read:${scope}=substitute;fs.write:${scope}=substitute` },
          expectedChecks: SQLITE_SUBSTITUTE_CHECKS,
          judge: () => {
            const problems = [];
            if (filesBelow(root).length !== 0) problems.push(`the real folder changed: ${filesBelow(root)}`);
            if (!filesBelow(join(home, 'shadow')).some(path => path.endsWith('app.db'))) problems.push('the database is not in the folder of the stand-in');
            return problems;
          },
        });
      } finally {
        rmSync(base, { recursive: true, force: true });
      }
    },

    // The module crypto against the OpenSSL of Node, the runner being the oracle and the page the subject.
    async crypto() {
      const oracle = cryptoOracle();
      return drive({
        name: 'crypto', app: 'modules/data/crypto', targets: oracle.targets, expectedChecks: CRYPTO_CHECKS,
        judge: lines => oracle.judge(lines),
      });
    },

    // The WebCrypto of the engine in the origin of an application: the detail of every check says whether the
    // algorithm is supported (run with --verbose to read the table); only the context and the algorithms an
    // application cannot do without fail the run.
    webcrypto: () => drive({ name: 'webcrypto', app: 'modules/data/webcrypto', expectedChecks: WEBCRYPTO_CHECKS }),

    // The secrets of an end-to-end run are in memory: the credential store of the machine is never touched.
    // The page works with the right allowed; with a stand-in the user chose; with the right denied.
    async secrets() {
      return drive({ name: 'secrets', app: 'modules/data/secrets', targets: { mode: 'allowed' }, expectedChecks: SECRETS_CHECKS });
    },

    async 'secrets-substitute'() {
      const home = mkdtempSync(join(os.tmpdir(), 'alef-e2e-secrets-'));
      try {
        return await drive({
          name: 'secrets-substitute', app: 'modules/data/secrets', targets: { mode: 'substituted' },
          env: { ALEF_HOME: home, ALEF_E2E_CONSENT: 'secrets=substitute' }, expectedChecks: SECRETS_SUBSTITUTE_CHECKS,
        });
      } finally {
        rmSync(home, { recursive: true, force: true });
      }
    },

    async 'secrets-denied'() {
      const home = mkdtempSync(join(os.tmpdir(), 'alef-e2e-secrets-'));
      try {
        return await drive({
          name: 'secrets-denied', app: 'modules/data/secrets', targets: { mode: 'denied' },
          env: { ALEF_HOME: home, ALEF_E2E_CONSENT: 'secrets=deny' }, expectedChecks: SECRETS_DENIED_CHECKS,
        });
      } finally {
        rmSync(home, { recursive: true, force: true });
      }
    },

    // The store keeps what was written for the next run: the application runs twice, a new process each
    // time, on a data folder that is empty at the start and removed at the end.
    async store() {
      const site = prepareSite('store', 'modules/data/store');
      const problems = [];
      const data = appDataOf(STORE_ID);
      const wipe = () => {
        if (data.endsWith(STORE_ID)) rmSync(data, { recursive: true, force: true });
      };
      const same = (left, right) => (process.platform === 'win32' ? left.toLowerCase() === right.toLowerCase() : left === right);
      const run = async step => {
        const running = startApp({ exe, args: ['--app', site, '--', `--step=${step}`], verbose });
        try {
          await running.waitFor(line => line.includes('ALEF_E2E RESULT'), 120000, `the verdict of ${step}`, { survivesExit: true });
          const result = verdictOf(running.lines);
          if (result?.[1] !== 'PASS') problems.push(`${step}: ${result?.[2] || 'the run failed'}`);
          const exit = await running.waitForExit(30000);
          if (exit?.code !== 0) problems.push(`${step}: the run ended with ${exit?.code ?? 'no exit'}`);
        } catch (error) {
          problems.push(`${step}: ${error.message}`);
        } finally {
          running.stop();
        }
        const done = running.lines.map(line => /ALEF_E2E check (\S+) ok/.exec(line)?.[1]).filter(Boolean);
        for (const name of STORE_CHECKS[step]) if (!done.includes(name)) problems.push(`${step}: no ok line for ${name}`);
        const reported = running.lines.map(line => /ALEF_E2E path appData (.+)$/.exec(line)?.[1]).find(Boolean);
        if (!reported || !same(reported.trim(), data)) problems.push(`${step}: the data folder is ${reported}, the runner expected ${data}`);
      };
      try {
        wipe();
        await run('write');
        await run('read');
        if (!existsSync(join(data, 'store'))) problems.push('the store is not in the data folder of the application');
      } finally {
        wipe();
      }
      return { problems, lines: [] };
    },
  };
}
