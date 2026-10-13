// SPDX-License-Identifier: MIT OR Apache-2.0
// The `package` scenario: the real `alef` binary runs real packages built by packages/tools.
// Positives pin a signer and keep state between runs; negative cases prove the exact refusal
// category and exit code of every broken package; on-disk proofs show that two signers of one
// id, and a directory run of the same id, keep separate data, consent and shadow locations.
import { spawnSync } from 'node:child_process';
import { copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import os from 'node:os';
import { basename, join } from 'node:path';
import { crc32, deflateRawSync, inflateRawSync } from 'node:zlib';

import { here, startApp, transpileApi, verdictOf } from '../../lib.mjs';
import { createKey, pack } from '../../../../packages/tools/src/index.mjs';

const ID = 'org.alef.e2e.package';
const PROBE = 'package-probe-42';

const PAGE = `// The page of the package scenario: what it does is chosen by targets.json.
import { api, guard, report, suite, verdict } from './harness.js';

const SERVICE = ${JSON.stringify(ID)};

const t = await (await fetch('targets.json')).json();

async function main() {
  const checks = suite();
  await report('appdata=' + await api.path.appData());
  await report('appconfig=' + await api.path.appConfig());
  await report('appcache=' + await api.path.appCache());
  await checks.check('the-page-is-served-from-the-package-archive', async () => {
    const probe = await (await fetch('/probe.txt')).text();
    if (probe !== t.probe) throw new Error('the probe read as ' + JSON.stringify(probe));
  });
  if (t.mode === 'first') {
    await checks.check('the-package-store-starts-empty-and-keeps-a-value', async () => {
      if (await api.store.get(t.key) !== undefined) throw new Error('the key was stored before the first run');
      await api.store.set(t.key, 'one');
    });
  }
  if (t.mode === 'recall') {
    await checks.check('the-second-run-reads-what-the-first-run-stored', async () => {
      if (await api.store.get(t.key) !== 'one') throw new Error('the stored value is gone');
    });
  }
  if (t.mode === 'secrets-write') {
    try {
      await api.secrets.set(SERVICE, 'probe', 'one');
      await api.secrets.set(SERVICE, 'canary', 'kept');
      await report('secrets-written');
      // Read the probe back in the same process: distinguishes a lazy store from cross-process loss.
      let back;
      try { back = await api.secrets.getText(SERVICE, 'probe'); } catch (error) { back = \`error: \${error?.code ?? error}\`; }
      await report('secrets-readback=' + JSON.stringify(back));
    } catch (error) {
      await report('secrets-unavailable');
    }
  }
  if (t.mode === 'secrets-read') {
    let value = null;
    let available = true;
    try {
      value = await api.secrets.get(SERVICE, 'probe');
    } catch (error) {
      available = false;
    }
    if (!available) await report('secrets-unavailable');
    else if (value !== null) await report('secrets-shared');
    else {
      await report('secrets-isolated');
      await api.secrets.set(SERVICE, 'probe', 'two');
      await api.secrets.delete(SERVICE, 'probe');
    }
  }
  if (t.mode === 'secrets-clean') {
    await checks.check('the-secret-of-the-first-key-survives-for-the-same-identity', async () => {
      // A store unreachable in this process is not a product failure: report and stop.
      let probe;
      try { probe = await api.secrets.getText(SERVICE, 'probe'); } catch (error) {
        await report('secrets-unavailable');
        return;
      }
      await report('secrets-plain-get=' + JSON.stringify(probe));
      let canary;
      try { canary = await api.secrets.getText(SERVICE, 'canary'); } catch (error) { canary = \`error: \${error?.code ?? error}\`; }
      await report('secrets-canary=' + JSON.stringify(canary));
      // Strict: both items must survive until this run; only then remove them.
      if (canary !== 'kept' || probe !== 'one') {
        throw new Error(\`the secret read as \${JSON.stringify(canary)}; probe as \${JSON.stringify(probe)}\`);
      }
      await api.secrets.delete(SERVICE, 'probe');
      await api.secrets.delete(SERVICE, 'canary');
    });
  }
  if (t.mode === 'shadow') {
    await checks.check('a-substituted-write-lands-in-the-shadow-of-this-identity', async () => {
      await api.fs.mkdir(t.shadowParent, { recursive: true });
      await api.fs.writeText(t.shadowProbe, 'stand-in');
    });
  }
  if (t.mode === 'hold') {
    await report('holding');
    return;
  }
  await verdict(checks.failed());
  await api.app.exit(t.exitCode ?? 0);
}

guard(main);
`;

const INDEX = `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Alef e2e: package</title>
  <script type="module" src="page.js"></script>
</head>
<body>
  <h1>Alef e2e: package</h1>
</body>
</html>
`;

/** The manifest of the site; the optional fields model the negative packages. */
function manifest({ withId = true, runtime = undefined, assets = undefined } = {}) {
  const lines = [];
  if (withId) lines.push(`id: ${ID}`);
  lines.push('name: Alef e2e package');
  lines.push('version:: 1.0.0');
  if (runtime !== undefined) lines.push(`runtime: ${runtime}`);
  if (assets !== undefined) lines.push(`assets: ${assets}`);
  lines.push(`windows: [
    {
        label: main
        url: /index.html
        width: 800
        height: 600
    }
]
external: {
    connect: []
    load: {
        scripts: []
        styles: []
        images: []
        fonts: []
        media: []
        frames: []
    }
}
permissions: {
    fs: {
        read: []
        write: [$TEMP/**]
    }
    cli: {
        exec: []
    }
    net: {
        http: []
        socket: []
    }
    shell: {
        openExternal: []
    }
    clipboard: {
        read: false
    }
    shortcut: {
        global: false
    }
    secrets: true
    app: {
        env: []
    }
}
`);
  return lines.join('\n');
}

/** 64 KiB of pseudo-random bytes (seeded LCG): incompressible, so the packer stores them. */
function blob() {
  const bytes = Buffer.alloc(65536);
  let seed = 0x2b6d9d3f;
  for (let index = 0; index < bytes.length; index += 1) {
    seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
    bytes[index] = seed >>> 24; // the low bits of this generator repeat every 256 bytes: the high ones do not
  }
  return bytes;
}

/** The central directory of a strict-profile ZIP: name -> { crc, size, method, localAt, centralAt }. */
function centralDirectory(bytes) {
  const end = bytes.length - 22;
  if (end < 0 || bytes.readUInt32LE(end) !== 0x06054b50) throw new Error('no end record');
  const count = bytes.readUInt16LE(end + 10);
  const records = new Map();
  let at = bytes.readUInt32LE(end + 16);
  for (let index = 0; index < count; index += 1) {
    if (bytes.readUInt32LE(at) !== 0x02014b50) throw new Error('no central signature');
    const nameLength = bytes.readUInt16LE(at + 28);
    const name = bytes.toString('utf8', at + 46, at + 46 + nameLength);
    records.set(name, {
      crc: bytes.readUInt32LE(at + 16),
      compressedSize: bytes.readUInt32LE(at + 20),
      size: bytes.readUInt32LE(at + 24),
      method: bytes.readUInt16LE(at + 10),
      localAt: bytes.readUInt32LE(at + 42),
      centralAt: at,
    });
    at += 46 + nameLength;
  }
  return records;
}

const LOCAL_NAME_LENGTH = 26; // uint16 at this offset in a local header
const LOCAL_CRC = 14; // uint32 in the local header
const CENTRAL_CRC = 16; // uint32 in the central directory header

/**
 * Rewrites one entry in place: `patch(data)` returns data of exactly the same length, and the
 * CRC-32 of both headers is fixed afterwards, so only the content itself is wrong. Returns the
 * (patched) entry data and its record.
 */
function patchEntry(bytes, name, patch) {
  const record = centralDirectory(bytes).get(name);
  if (record === undefined) throw new Error(`no entry ${name}`);
  const local = record.localAt;
  if (bytes.readUInt32LE(local) !== 0x04034b50) throw new Error('no local signature');
  const dataAt = local + 30 + bytes.readUInt16LE(local + LOCAL_NAME_LENGTH);
  const data = bytes.subarray(dataAt, dataAt + record.size);
  const replacement = patch(data);
  if (replacement.length !== record.size) throw new Error('the patch changed the length');
  const crc = crc32(replacement);
  bytes.writeUInt32LE(crc, local + LOCAL_CRC);
  bytes.writeUInt32LE(crc, record.centralAt + CENTRAL_CRC);
  return { record, data: replacement };
}

/** Flips one byte of a stored entry and repairs both CRC fields. */
const corruptByte = (bytes, name, index) => patchEntry(bytes, name, data => {
  data[index] ^= 0xff;
  return data;
});

export function packageScenarios({ exe, verbose, timeoutMs }) {
  /** The XDG bases, set by the scenario and shared by every home so the isolation comparison stays meaningful. */
  const bases = {};
  const xdg = () => process.platform === 'linux'
    ? { XDG_DATA_HOME: bases.data, XDG_CONFIG_HOME: bases.config, XDG_CACHE_HOME: bases.cache }
    : {};

  const environment = home => ({
    ALEF_HOME: home,
    ALEF_E2E_CONSENT: '*=allow',
    ...xdg(),
  });

  /** One `alef` run driven by a page: waits for the verdict and the exit, both bounded. */
  const runPage = async (pkg, home, args, extraEnv = {}) => {
    const running = startApp({ exe, verbose, args: ['run', pkg, ...args], env: { ...environment(home), ...extraEnv } });
    try {
      await running.waitFor(line => line.includes('ALEF_READY'), timeoutMs, 'ALEF_READY');
      await running.waitFor(line => line.includes('ALEF_E2E RESULT'), timeoutMs, 'the verdict of the page');
      const exit = await running.waitForExit(timeoutMs);
      return { lines: running.lines, code: exit?.code };
    } finally {
      running.stop();
    }
  };

  /** A page run kept alive on purpose (mode hold): the caller stops it. */
  const holdPage = async (pkg, home, args = []) => {
    const running = startApp({ exe, verbose, args: ['run', pkg, ...args], env: environment(home) });
    await running.waitFor(line => line.includes('ALEF_READY'), timeoutMs, 'ALEF_READY');
    await running.waitFor(line => line.includes('holding'), timeoutMs, 'the page to hold');
    return running;
  };

  /** A bounded page run with the exact command line given (for the `--app` spelling). */
  const runArgs = async (args, home) => {
    const running = startApp({ exe, verbose, args, env: environment(home) });
    try {
      await running.waitFor(line => line.includes('ALEF_E2E RESULT'), timeoutMs, 'the verdict of the page');
      const exit = await running.waitForExit(timeoutMs);
      return { lines: running.lines, code: exit?.code };
    } finally {
      running.stop();
    }
  };

  const stop = async running => {
    running.stop();
    await running.waitForExit(30000);
  };

  const cli = (args, home, extra = {}) => {
    const result = spawnSync(exe, args, {
      env: { ...process.env, ALEF_E2E: '1', ALEF_E2E_CONSENT: '*=allow', ...xdg(), ALEF_HOME: home, ...extra },
      encoding: 'utf8', timeout: 60000,
    });
    return { code: result.status, out: result.stdout ?? '', err: result.stderr ?? '' };
  };

  const okNames = lines => new Set(lines.map(line => /check (\S+) ok /.exec(line)?.[1]).filter(Boolean));
  const reported = (lines, prefix) => {
    const found = lines.find(candidate => candidate.includes(`ALEF_E2E ${prefix}=`));
    return found === undefined ? undefined : found.slice(found.indexOf(`${prefix}=`) + prefix.length + 1);
  };

  return {
    package: async () => {
      const problems = [];
      const expect = (what, condition, detail = '') => {
        console.log(`    ${condition ? 'ok  ' : 'FAIL'} ${what}${condition ? '' : `: ${detail}`}`);
        if (!condition) problems.push(`${what}: ${detail}`);
      };
      // Paths the pages reported under the OS data bases; removed in the finally block.
      const created = new Set();
      const track = path => { if (path) created.add(path); };

      const dir = mkdtempSync(join(os.tmpdir(), 'alef-e2e-package-'));
      const pkgs = join(dir, 'pkgs');
      mkdirSync(pkgs, { recursive: true });
      const probeFolder = join(dir, 'shadow-probes');
      mkdirSync(probeFolder, { recursive: true });
      const homeA = join(dir, 'home-a');
      const homeB = join(dir, 'home-b');
      const keyHomeA = join(dir, 'keys-a');
      const keyHomeB = join(dir, 'keys-b');
      const keyHomeC = join(dir, 'keys-c');
      // One shared base for every run: the isolation comparison below must be meaningful.
      for (const name of ['data', 'config', 'cache']) {
        bases[name] = join(dir, 'xdg', name);
        mkdirSync(bases[name], { recursive: true });
      }

      const fingerprint = {};
      let pinnedFingerprint;
      let blobRecord;
      try {
        // Keys and packages.
        const keyA = await createKey('a', { home: keyHomeA });
        const keyB = await createKey('b', { home: keyHomeB });
        await createKey('c', { home: keyHomeC });
        fingerprint.a = keyA.fingerprint;
        fingerprint.b = keyB.fingerprint;

        const api = transpileApi();
        const buildSite = (name, targets, manifestOptions = {}) => {
          const site = join(dir, 'sites', name);
          const publicRoot = join(site, 'dist');
          mkdirSync(publicRoot, { recursive: true });
          writeFileSync(join(site, 'alef.ktav'), manifest(manifestOptions));
          writeFileSync(join(publicRoot, 'index.html'), INDEX);
          writeFileSync(join(publicRoot, 'page.js'), PAGE);
          writeFileSync(join(publicRoot, 'probe.txt'), PROBE);
          writeFileSync(join(publicRoot, 'blob.bin'), blob());
          copyFileSync(join(here, 'harness.js'), join(publicRoot, 'harness.js'));
          for (const folder of ['src', 'types']) cpSync(join(api, folder), join(publicRoot, folder), { recursive: true });
          writeFileSync(join(publicRoot, 'targets.json'), JSON.stringify({ probe: PROBE, shadowParent: probeFolder, shadowProbe: join(probeFolder, 'probe.txt'), ...targets }));
          return site;
        };
        const buildPackage = async (name, targets, { key, unsigned = false, manifestOptions = {} } = {}) => {
          const site = buildSite(name, targets, manifestOptions);
          const out = join(pkgs, `${name}.alef`);
          const result = await pack(site, unsigned ? { unsigned: true, out } : { home: key === 'b' ? keyHomeB : keyHomeA, key, out });
          return { site, pkg: out, fingerprint: result.fingerprint };
        };

        const firstA = await buildPackage('signed-a-first', { mode: 'first', key: 'visit', exitCode: 7 }, { key: 'a' });
        pinnedFingerprint = firstA.fingerprint;
        if (pinnedFingerprint !== fingerprint.a) throw new Error('pack returned an unexpected fingerprint');
        const recallA = await buildPackage('signed-a-recall', { mode: 'recall', key: 'visit' }, { key: 'a' });
        const pathsA = await buildPackage('signed-a-paths', { mode: 'paths' }, { key: 'a' });
        const holdA = await buildPackage('signed-a-hold', { mode: 'hold' }, { key: 'a' });
        const secretsWriteA = await buildPackage('signed-a-secrets-write', { mode: 'secrets-write' }, { key: 'a' });
        const secretsCleanA = await buildPackage('signed-a-secrets-clean', { mode: 'secrets-clean' }, { key: 'a' });
        const firstB = await buildPackage('signed-b-first', { mode: 'first', key: 'visit', exitCode: 7 }, { key: 'b' });
        const secretsReadB = await buildPackage('signed-b-secrets-read', { mode: 'secrets-read' }, { key: 'b' });
        const unsignedU = await buildPackage('unsigned-u-first', { mode: 'first', key: 'unsigned-visit' }, { unsigned: true });
        const noId = await buildPackage('noid', { mode: 'paths' }, { key: 'a', manifestOptions: { withId: false } });
        const runtime = await buildPackage('runtime', { mode: 'paths' }, { key: 'a', manifestOptions: { runtime: '>=99.0' } });
        const noAssets = await buildPackage('noassets', { mode: 'paths' }, { key: 'a', manifestOptions: { assets: 'nodist' } });
        const shadowPackageA = await buildPackage('shadow-a', { key: 'a', mode: 'shadow' }, { key: 'a' });
        const shadowPackageB = await buildPackage('shadow-b', { key: 'b', mode: 'shadow' }, { key: 'b' });

        // The corruption cases start from the exact bytes of the first signed package.
        const pristine = readFileSync(firstA.pkg);
        blobRecord = centralDirectory(pristine).get('dist/blob.bin');
        if (blobRecord === undefined) throw new Error('dist/blob.bin is missing from the package');
        if (blobRecord.method !== 0) throw new Error('blob.bin was deflated; the CRC case needs a stored entry');

        const corruptCrc = Buffer.from(pristine);
        corruptByte(corruptCrc, 'dist/blob.bin', 0);
        const crcPath = join(pkgs, 'corrupt-crc.alef');
        writeFileSync(crcPath, corruptCrc);

        // The signature case rewrites the SHA-256 line of blob.bin. META-INF/hashes is deflated,
        // so its replacement stream must have exactly the old length; the reader verifies the
        // signature over the hash list before the payload pass, so this yields PACKAGE_SIGNATURE.
        const signaturePath = join(pkgs, 'corrupt-signature.alef');
        const buildSignature = blobIndex => {
          const bytes = Buffer.from(pristine);
          const patched = corruptByte(bytes, 'dist/blob.bin', blobIndex);
          const newHash = createHash('sha256').update(patched.data).digest('hex');
          const record = centralDirectory(bytes).get('META-INF/hashes');
          if (record === undefined) throw new Error('META-INF/hashes is missing');
          const local = record.localAt;
          if (bytes.readUInt32LE(local) !== 0x04034b50) throw new Error('no local signature');
          if (bytes.readUInt16LE(local + 28) !== 0) throw new Error('unexpected local extra field');
          const dataAt = local + 30 + bytes.readUInt16LE(local + LOCAL_NAME_LENGTH);
          const raw = inflateRawSync(bytes.subarray(dataAt, dataAt + record.compressedSize));
          const text = raw.toString('latin1');
          const line = /([0-9a-f]{64})(  dist\/blob\.bin\n)/.exec(text);
          if (line === null) throw new Error('the hash list has no line for blob.bin');
          // The whole list is deflated again; some level yields a stream of the old length.
          const edited = Buffer.from(`${text.slice(0, line.index)}${newHash}${text.slice(line.index + 64)}`, 'latin1');
          let replacement = null;
          for (let level = 1; level <= 9 && replacement === null; level += 1) {
            const candidate = deflateRawSync(edited, { level });
            if (candidate.length === record.compressedSize) replacement = candidate;
          }
          if (replacement === null) return null;
          bytes.subarray(dataAt, dataAt + record.compressedSize).set(replacement);
          const crc = crc32(edited);
          bytes.writeUInt32LE(crc, local + LOCAL_CRC);
          bytes.writeUInt32LE(crc, record.centralAt + CENTRAL_CRC);
          return bytes;
        };
        let corruptSignature = null;
        for (let blobIndex = 1; blobIndex < 64 && corruptSignature === null; blobIndex += 1) {
          corruptSignature = buildSignature(blobIndex);
        }
        if (corruptSignature === null) throw new Error('no same-length deflate replacement of META-INF/hashes found');
        writeFileSync(signaturePath, corruptSignature);

        const plain = join(dir, 'plain.txt');
        writeFileSync(plain, 'this is not a package');

        // Positive 1: signed, first run, trust given on the command line, page asks for exit 7.
        const first = await runPage(firstA.pkg, homeA, ['--trust-key', fingerprint.a]);
        track(reported(first.lines, 'appdata'));
        track(reported(first.lines, 'appconfig'));
        track(reported(first.lines, 'appcache'));
        expect('the first signed run starts and asks for exit 7', first.code === 7, `exit ${first.code}`);
        expect('the first signed run passes its checks', (() => {
          const verdict = verdictOf(first.lines);
          return verdict?.[1] === 'PASS' && ['the-page-is-served-from-the-package-archive', 'the-package-store-starts-empty-and-keeps-a-value']
            .every(name => okNames(first.lines).has(name));
        })(), verdictOf(first.lines)?.[2] ?? 'no verdict');

        // Positive 2: the pin answers the second run by itself, and the store survived.
        const recall = await runPage(recallA.pkg, homeA, []);
        expect('the second run needs no trust answer and reads the store', recall.code === 0
          && okNames(recall.lines).has('the-second-run-reads-what-the-first-run-stored'), `exit ${recall.code}`);

        // Positive 3: exactly one pin file, named by the id hash, naming the fingerprint.
        const trustFolder = join(homeA, 'trust');
        const pinName = `${createHash('sha256').update(ID).digest('hex')}.json`;
        const pinPath = join(trustFolder, pinName);
        expect('the pin file names the id hash and the fingerprint',
          existsSync(pinPath) && readFileSync(pinPath, 'utf8').includes(fingerprint.a),
          `${pinPath} (${fingerprint.a})`);
        expect('the trust folder holds exactly the one pin', existsSync(trustFolder) && readdirSync(trustFolder).length === 1
          && readdirSync(trustFolder)[0] === pinName, JSON.stringify(existsSync(trustFolder) ? readdirSync(trustFolder) : null));

        // Positive 4: the alias spelling of the same pinned package.
        const alias = await runArgs(['--app', pathsA.pkg], homeA);
        expect('the --app spelling runs the pinned package', alias.code === 0, `exit ${alias.code}`);

        // Positive 5: an unsigned package with --allow-unsigned pins nothing.
        const homeU = join(dir, 'home-unsigned');
        const unsigned = await runPage(unsignedU.pkg, homeU, ['--allow-unsigned']);
        expect('the unsigned run starts with --allow-unsigned and keeps its own store', unsigned.code === 0
          && okNames(unsigned.lines).has('the-package-store-starts-empty-and-keeps-a-value'), `exit ${unsigned.code}`);
        expect('an unsigned run writes no pin', !existsSync(join(homeU, 'trust'))
          || readdirSync(join(homeU, 'trust')).length === 0, 'the trust folder is not empty');

        // Positive 6: a wrong --trust-key is an argument error before anything starts.
        const homeW = join(dir, 'home-wrong');
        const wrong = cli(['run', firstA.pkg, '--trust-key', 'a'.repeat(64)], homeW);
        expect('a wrong --trust-key is refused as an invalid argument', wrong.code === 2
          && wrong.err.includes('INVALID_ARGUMENT') && wrong.err.includes('does not match'), `exit ${wrong.code}: ${wrong.err}`);
        expect('a wrong --trust-key starts nothing and pins nothing', !wrong.out.includes('ALEF_READY') && !wrong.err.includes('ALEF_READY')
          && !existsSync(join(homeW, 'trust')), wrong.out + wrong.err);

        // The negative packages: exact category, exact exit code, and never a start.
        const negative = (what, args, home, category, code, needles = [], extra = {}) => {
          const outcome = cli(args, home, extra);
          const log = outcome.out + outcome.err;
          expect(what, outcome.code === code && outcome.err.includes(category)
            && needles.every(needle => outcome.err.includes(needle)) && !log.includes('ALEF_READY'),
            `exit ${outcome.code}: ${outcome.err}`);
        };
        negative('a plain text file is not a package', ['run', plain], join(dir, 'home-n1'), 'NOT_A_PACKAGE', 2);
        negative('a flipped byte with a fixed CRC is an integrity failure', ['run', crcPath], join(dir, 'home-n2'), 'PACKAGE_INTEGRITY', 2, ['SHA-256 mismatch']);
        negative('a rewritten hash list is a signature failure', ['run', signaturePath], join(dir, 'home-n3'), 'PACKAGE_SIGNATURE', 2);
        negative('a manifest without an id is invalid', ['run', noId.pkg], join(dir, 'home-n4'), 'MANIFEST_INVALID', 2);
        negative('an unmet runtime requirement is refused', ['run', runtime.pkg], join(dir, 'home-n5'), 'RUNTIME_INCOMPATIBLE', 2);
        negative('a missing assets root is invalid', ['run', noAssets.pkg], join(dir, 'home-n6'), 'MANIFEST_INVALID', 2);

        const homeR = join(dir, 'home-refuse');
        negative('a refused trust question ends declined', ['run', firstA.pkg], homeR, 'TRUST_REFUSED', 3, [], { ALEF_E2E_TRUST: 'refuse' });
        expect('a refused launch pins nothing', !existsSync(join(homeR, 'trust')) || readdirSync(join(homeR, 'trust')).length === 0);

        // The second signer of the same id in the pinned home: declined, and the pin survives.
        negative('a package of another signer is declined in a pinned home', ['run', firstB.pkg], homeA, 'SIGNER_CHANGED', 3);
        expect('the pin still names the first fingerprint after the refusal',
          existsSync(pinPath) && readFileSync(pinPath, 'utf8').includes(fingerprint.a));
        negative('an unsigned package in a pinned home asks for a signature', ['run', unsignedU.pkg, '--allow-unsigned'], homeA, 'SIGNATURE_REQUIRED', 3);

        // Endpoint isolation: the held run of key a must not swallow the start of key b.
        const held = await holdPage(holdA.pkg, homeA);
        let second;
        try {
          second = await runPage(firstB.pkg, homeB, ['--trust-key', fingerprint.b]);
        } finally {
          await stop(held);
        }
        track(reported(second.lines, 'appdata'));
        track(reported(second.lines, 'appconfig'));
        track(reported(second.lines, 'appcache'));
        expect('the-single-instance-endpoint-is-per-authority-not-per-id',
          second.code === 7 && verdictOf(second.lines)?.[1] === 'PASS'
          && okNames(second.lines).has('the-package-store-starts-empty-and-keeps-a-value'),
          `exit ${second.code}: ${verdictOf(second.lines)?.[2] ?? 'no verdict'}`);

        // Data/cache isolation of the two signers over the one shared XDG base.
        const appdataA = reported(first.lines, 'appdata');
        const appdataB = reported(second.lines, 'appdata');
        const appcacheA = reported(first.lines, 'appcache');
        const appcacheB = reported(second.lines, 'appcache');
        expect('two-signers-of-one-id-keep-separate-data-directories',
          [appdataA, appdataB].every(path => path !== undefined && path.includes('alef-packages'))
          && basename(appdataA) !== basename(appdataB)
          && [appcacheA, appcacheB].every(path => path !== undefined && path.includes('alef-packages'))
          && basename(appcacheA) !== basename(appcacheB),
          `${appdataA} vs ${appdataB}; ${appcacheA} vs ${appcacheB}`);
        const folderNames = (home, name) => existsSync(join(home, name)) ? readdirSync(join(home, name)) : [];
        const consents = [folderNames(homeA, 'consent'), folderNames(homeB, 'consent')];
        expect('two-signers-of-one-id-keep-separate-consent-decisions',
          consents.every(names => names.length > 0) && consents[0].every(name => !consents[1].includes(name)),
          JSON.stringify(consents));
        // The shadow is only created by a substituted write, and a remembered decision is not asked
        // again: each signer gets a fresh home, so the substitute answer is really given.
        const homeShadowA = join(dir, 'home-shadow-a');
        const homeShadowB = join(dir, 'home-shadow-b');
        const shadowConsent = { ALEF_E2E_CONSENT: 'fs.write:$TEMP/**=substitute;*=allow' };
        const shadowRunA = await runPage(shadowPackageA.pkg, homeShadowA, ['--trust-key', fingerprint.a], shadowConsent);
        const shadowRunB = await runPage(shadowPackageB.pkg, homeShadowB, ['--trust-key', fingerprint.b], shadowConsent);
        const shadows = [folderNames(homeShadowA, 'shadow'), folderNames(homeShadowB, 'shadow')];
        // Both signers target the same real path; only their own shadow may receive the write.
        const shadowProbes = home => {
          const probes = [];
          let remaining = 256;
          const visit = (folder, depth) => {
            if (depth > 32) throw new Error('shadow probe traversal exceeded 32 levels');
            for (const entry of readdirSync(folder, { withFileTypes: true })) {
              if (--remaining < 0) throw new Error('shadow probe traversal exceeded 256 entries');
              const path = join(folder, entry.name);
              if (entry.isDirectory()) visit(path, depth + 1);
              else if (entry.isFile() && entry.name === 'probe.txt') probes.push(path);
            }
          };
          const root = join(home, 'shadow');
          if (existsSync(root)) visit(root, 0);
          return probes;
        };
        const probes = [shadowProbes(homeShadowA), shadowProbes(homeShadowB)];
        expect('two-signers-of-one-id-keep-separate-shadow-identities',
          shadowRunA.code === 0 && shadowRunB.code === 0
          && [shadowRunA, shadowRunB].every(run => verdictOf(run.lines)?.[1] === 'PASS'
            && okNames(run.lines).has('a-substituted-write-lands-in-the-shadow-of-this-identity'))
          && shadows.every(names => names.length > 0) && shadows[0].every(name => !shadows[1].includes(name))
          && probes.every(paths => paths.length > 0 && paths.every(path => readFileSync(path, 'utf8') === 'stand-in'))
          && !existsSync(join(probeFolder, 'probe.txt')),
          `exit ${shadowRunA.code}/${shadowRunB.code}: page verdicts ${JSON.stringify([shadowRunA, shadowRunB].map(run => verdictOf(run.lines)?.slice(1) ?? 'no verdict'))}; shadows ${JSON.stringify(shadows)}; probes ${JSON.stringify(probes)}; real probe exists: ${existsSync(join(probeFolder, 'probe.txt'))}`);

        // Secrets isolation: the second signer never reads what the first one wrote. The secrets part
        // always runs; the e2e backends keep secrets per process, so on machines without a persistent
        // store the survival part reports SKIP. A vanishing probe with a surviving canary is a product
        // suspicion and fails.
        const write = await runPage(secretsWriteA.pkg, homeA, []);
        const written = write.code === 0 && write.lines.some(line => line.includes('secrets-written'));
        expect('the first signer stores its secret or reports no store', write.code === 0
          && (written || write.lines.some(line => line.includes('secrets-unavailable'))), `exit ${write.code}`);
        const read = await runPage(secretsReadB.pkg, homeB, []);
        expect('the second signer never reads the secret of the first',
          read.code === 0 && read.lines.some(line => line.includes('secrets-isolated') || line.includes('secrets-unavailable'))
          && !read.lines.some(line => line.includes('secrets-shared')), `exit ${read.code}`);
        if (written) {
          const readbackOk = write.lines.some(line => line.includes('secrets-readback="one"'));
          const clean = await runPage(secretsCleanA.pkg, homeA, []);
          const plainGet = reported(clean.lines, 'secrets-plain-get');
          const canary = reported(clean.lines, 'secrets-canary');
          const cleanOk = clean.code === 0 && okNames(clean.lines).has('the-secret-of-the-first-key-survives-for-the-same-identity');
          if (clean.lines.some(line => line.includes('secrets-unavailable'))) {
            console.log('    SKIP secrets-survival: the secret store is not available in this process');
          } else if (canary !== '"kept"' && plainGet !== '"one"') {
            console.log('    SKIP secrets-survival: the secret store of this machine does not keep items between processes (the isolation check by the second signer is vacuous)');
          } else if (canary === '"kept"' && plainGet !== '"one"') {
            expect('the-secret-of-the-first-key-survives-for-the-same-identity', false,
              `exit ${clean.code}; plainGet=${plainGet}; canary=${canary}; cause: the secret of the first signer vanished while a canary nobody touched survived (product suspicion: the delete or the lookup of the second signer removed it)`);
          } else {
            // Name the likely cause from the probes when the strict check fails.
            const cause = !readbackOk
              ? 'cause: the store did not keep the secret inside the writing process'
              : 'cause: the stored value changed';
            expect('the-secret-of-the-first-key-survives-for-the-same-identity', cleanOk,
              `exit ${clean.code}; ${cause}; plainGet=${plainGet}; canary=${canary}`);
          }
        }

        // A directory run of the same id keeps the legacy location.
        const homeD = join(dir, 'home-directory');
        const legacy = await runPage(pathsA.site, homeD, []);
        track(reported(legacy.lines, 'appdata'));
        track(reported(legacy.lines, 'appconfig'));
        track(reported(legacy.lines, 'appcache'));
        const legacyData = reported(legacy.lines, 'appdata');
        expect('a-directory-run-of-the-same-id-keeps-its-legacy-data-location',
          legacy.code === 0 && legacyData !== undefined && !legacyData.includes('alef-packages')
          && basename(legacyData) === ID, `exit ${legacy.code}: ${legacyData}`);
      } catch (error) {
        problems.push(error.message);
      } finally {
        for (const path of created) rmSync(path, { recursive: true, force: true });
        rmSync(dir, { recursive: true, force: true });
      }
      return { problems, lines: [] };
    },
  };
}
