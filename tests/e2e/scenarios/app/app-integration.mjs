// SPDX-License-Identifier: MIT OR Apache-2.0
// Real native entries only: independent OS reads, runner/page checkpoints, owned crash journal.
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { prepareSite, scratch, startApp, verdictOf } from '../../lib.mjs';

const journalRoot = join(dirname(scratch), 'e2e-app-integration-owned');
const runKey = 'Software\\Microsoft\\Windows\\CurrentVersion\\Run';
const bound = 30000;
const expect = (ok, message) => { if (!ok) throw new Error(message); };
function command(exe, args) {
  const result = spawnSync(exe, args, { encoding: 'utf8', timeout: 10000, killSignal: 'SIGKILL', maxBuffer: 128 * 1024, windowsHide: true });
  if (result.error || result.status !== 0) throw new Error(`${exe}: ${result.error?.message ?? result.stderr ?? result.status}`);
  return result.stdout.trim();
}
const psString = value => `'${value.replaceAll("'", "''")}'`;
function powershell(body) {
  return command('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(`$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); ${body}`, 'utf16le').toString('base64')]);
}
const quote = value => `"${value.replace(/(\\*)"/g, '$1$1\\"').replace(/(\\+)$/g, '$1$1')}"`;
const desktopArg = value => `"${value.replaceAll('%', '%%').replace(/[\\"`$]/g, '\\$&')}"`.replaceAll('\\', '\\\\');
const xml = value => value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;').replaceAll("'", '&apos;');
function canonicalShellPath(path) {
  const resolved = realpathSync.native(path);
  if (process.platform !== 'win32') return resolved;
  if (resolved.startsWith('\\\\?\\UNC\\')) return `\\\\${resolved.slice(8)}`;
  if (resolved.startsWith('\\\\?\\')) {
    const drive = resolved.slice(4);
    expect(/^[A-Za-z]:\\/.test(drive), 'unsupported verbatim shell path');
    return drive;
  }
  return resolved;
}
function identity(id, scheme, site, exe, kind) {
  const name = `alef-${Buffer.from(id).toString('hex')}`;
  const home = process.env.HOME || homedir();
  const config = process.env.XDG_CONFIG_HOME || join(home, '.config');
  const args = [canonicalShellPath(exe), '--app', canonicalShellPath(site)];
  const win = args.map(quote).join(' ');
  const desktop = `[Desktop Entry]\nType=Application\nName=${name}\nExec=${args.map(desktopArg).join(' ')}\nTerminal=false\n`;
  const plist = `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict><key>Label</key><string>${name}</string><key>ProgramArguments</key><array>${args.map(a => `<string>${xml(a)}</string>`).join('')}</array><key>RunAtLoad</key><true/></dict></plist>\n`;
  return { version: 1, platform: process.platform, pid: process.pid, id, scheme, site: args[2], exe: args[0], kind, name, home, config, win, desktop, plist };
}
function paths(j) {
  const applications = join(j.home, '.local', 'share', 'applications');
  return {
    entry: j.kind === 'deeplink' ? join(applications, `${j.name}.desktop`) : process.platform === 'darwin' ? join(j.home, 'Library', 'LaunchAgents', `${j.name}.plist`) : join(j.config, 'autostart', `${j.name}.desktop`),
    backup: join(applications, `${j.name}.${j.scheme}.previous`),
    defaults: join(j.config, 'mimeapps.list'),
  };
}
const expectedFile = j => j.kind === 'deeplink' ? `${j.desktop.replace('\nTerminal=false', ' %u\nTerminal=false')}MimeType=x-scheme-handler/${j.scheme};\n` : process.platform === 'darwin' ? j.plist : j.desktop;
function registry(j) {
  const key = j.kind === 'autostart' ? runKey : `Software\\Classes\\${j.scheme}`;
  const body = j.kind === 'autostart'
    ? `$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey(${psString(key)}); if($k){$v=$k.GetValue(${psString(j.name)},$null); $k.Dispose(); $v | ConvertTo-Json -Compress}else{'null'}`
    : `$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey(${psString(key)}); if(!$k){'null'}else{$c=$k.OpenSubKey('shell\\open\\command'); @{owner=$k.GetValue('AlefOwner'); protocol=$k.GetValue('URL Protocol',$null); command=$(if($c){$c.GetValue('')})} | ConvertTo-Json -Compress; if($c){$c.Dispose()}; $k.Dispose()}`;
  return JSON.parse(powershell(body) || 'null');
}
function verify(j, enabled) {
  if (j.kind === 'deeplink' && process.platform === 'darwin') {
    console.log('    macOS deep registration: NOT_AVAILABLE (page must assert; no native entry)');
    return;
  }
  if (process.platform === 'win32') {
    const value = registry(j);
    if (!enabled) expect(value === null, 'native registry entry remains');
    else if (j.kind === 'autostart') expect(value === j.win, 'HKCU Run command differs from the canonical launch');
    else expect(value?.owner === `${j.name}|${j.site}` && value.protocol === '' && value.command === `${j.win} "%1"`, 'HKCU Classes owner/protocol/command mismatch');
  } else if (!(j.kind === 'deeplink' && process.platform === 'darwin')) {
    const { entry, backup } = paths(j);
    expect(enabled ? existsSync(entry) && readFileSync(entry, 'utf8') === expectedFile(j) : !existsSync(entry), `native entry ${enabled ? 'missing/incorrect' : 'remains'}: ${entry}`);
    if (j.kind === 'deeplink') {
      const selected = command('xdg-mime', ['query', 'default', `x-scheme-handler/${j.scheme}`]);
      expect(enabled ? selected === `${j.name}.desktop` : selected !== `${j.name}.desktop`, 'xdg default mismatch');
      if (!enabled) expect(!existsSync(backup), 'xdg backup remains');
    }
  }
  console.log(`    independent native ${j.kind}: ${enabled ? 'registered' : 'removed'} (${j.id}, ${j.scheme})`);
}
function removeExact(path, expected) {
  if (!existsSync(path)) return;
  expect(lstatSync(path).isFile() && readFileSync(path, 'utf8') === expected, `refusing foreign entry: ${path}`);
  rmSync(path);
}
function cleanup(j) {
  if (process.platform === 'win32') {
    if (j.kind === 'autostart') {
      powershell(`$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey(${psString(runKey)},$true); if($k){try{$v=$k.GetValue(${psString(j.name)},$null); if($null -ne $v){if($v -ne ${psString(j.win)}){throw 'foreign Run value'}; $k.DeleteValue(${psString(j.name)},$false)}}finally{$k.Dispose()}}`);
    } else {
      // Refuse unexpected values/children before removing our four-key skeleton bottom-up.
      const key = `Software\\Classes\\${j.scheme}`;
      powershell(`$root=[Microsoft.Win32.Registry]::CurrentUser; $p=${psString(key)}; $k=$root.OpenSubKey($p); if($k){$owner=$k.GetValue('AlefOwner'); $k.Dispose(); if($owner -ne ${psString(`${j.name}|${j.site}`)}){throw 'foreign scheme'}; $suffixes=@('','\\shell','\\shell\\open','\\shell\\open\\command'); for($i=0;$i -lt 4;$i++){$k=$root.OpenSubKey($p+$suffixes[$i]); if($k){try{$values=@($k.GetValueNames()); $children=@($k.GetSubKeyNames()); $allowedValues=@(); $allowedChildren=@(); if($i -eq 0){$allowedValues=@('AlefOwner','URL Protocol'); $allowedChildren=@('shell'); $protocol=$k.GetValue('URL Protocol',$null); if($null -ne $protocol -and $protocol -ne ''){throw 'foreign protocol'}}elseif($i -eq 1){$allowedChildren=@('open')}elseif($i -eq 2){$allowedChildren=@('command')}else{$allowedValues=@(''); $cmd=$k.GetValue('',$null); if($null -ne $cmd -and $cmd -ne ${psString(`${j.win} "%1"`)}){throw 'foreign command'}}; foreach($v in $values){if($v -notin $allowedValues){throw 'foreign value'}}; foreach($v in $children){if($v -notin $allowedChildren){throw 'foreign child'}}}finally{$k.Dispose()}}}; for($i=3;$i -ge 0;$i--){$root.DeleteSubKey($p+$suffixes[$i],$false)}}`);
    }
  } else if (!(j.kind === 'deeplink' && process.platform === 'darwin')) {
    const { entry, backup, defaults } = paths(j);
    const expected = expectedFile(j);
    if (j.kind === 'deeplink') {
      if (existsSync(backup)) {
        const saved = readFileSync(backup, 'utf8');
        expect(saved === `${expected}\nPrevious=\n`, 'refusing unexpected previous scheme association');
        if (existsSync(defaults)) {
          expect(lstatSync(defaults).isFile(), 'foreign mimeapps symlink');
          const mime = `x-scheme-handler/${j.scheme}=${j.name}.desktop`;
          const old = readFileSync(defaults, 'utf8');
          let section = false;
          const next = old.split(/(?<=\n)/).filter(raw => {
            const line = raw.replace(/[\r\n]+$/, '');
            if (line.startsWith('[')) section = line === '[Default Applications]';
            return !(section && (line === mime || line === `${mime};`));
          }).join('');
          if (next !== old) {
            expect(readFileSync(defaults, 'utf8') === old, 'mimeapps changed during owned cleanup');
            writeFileSync(defaults, next);
          }
        }
        removeExact(backup, saved);
      }
    }
    // Remove an exact owned entry even when xdg failed before writing a backup.
    removeExact(entry, expected);
  }
  verify(j, false);
}
function recover() {
  mkdirSync(journalRoot, { recursive: true });
  for (const name of readdirSync(journalRoot)) {
    if (!/^[0-9a-f-]{36}\.json$/.test(name)) continue;
    const file = join(journalRoot, name);
    const j = JSON.parse(readFileSync(file, 'utf8'));
    expect(j.version === 1 && j.platform === process.platform && /^org\.alef\.e2e\.native\.[0-9a-f]{32}$/.test(j.id) && /^alefe2e[0-9a-f]{32}$/.test(j.scheme) && ['autostart', 'deeplink'].includes(j.kind), 'invalid owned journal');
    // Another runner's live journal is never touched.
    try { process.kill(j.pid, 0); continue; } catch (error) { if (error.code !== 'ESRCH') throw error; }
    expect(j.name === `alef-${Buffer.from(j.id).toString('hex')}`, 'journal identity mismatch');
    expect(j.home === (process.env.HOME || homedir()) && j.config === (process.env.XDG_CONFIG_HOME || join(j.home, '.config')), 'journal belongs to a different home');
    cleanup(j);
    rmSync(file);
    console.log(`    recovered exact owned journal ${j.id}`);
  }
}
async function stop(running) {
  if (!running) return;
  if (!running.exit) running.stop();
  if (!(await running.waitForExit(5000))) { process.kill(running.pid, 'SIGKILL'); expect(await running.waitForExit(5000), 'owned child did not exit after kill'); }
  expect(await running.waitForClosed(5000), 'owned child pipes did not close');
}
function checkPage(running, checks, label) {
  expect(running.lines.filter(line => line.includes('ALEF_E2E RESULT')).length === 1, `${label}: expected exactly one verdict`);
  const result = verdictOf(running.lines);
  expect(result?.[1] === 'PASS', `${label} page failed: ${result?.[2]}`);
  expect(!running.lines.some(line => /check \S+ FAILED(?:\s|$)/.test(line)), `${label}: a page check failed`);
  for (const check of checks) {
    expect(running.lines.filter(line => line.includes(`check ${check} ok `)).length === 1, `${label}: missing or duplicate check ${check}`);
  }
}
export function appIntegrationScenarios({ exe, verbose, timeoutMs }) {
  const wait = Math.min(timeoutMs, 90000);
  async function run(kind) {
    recover();
    const token = randomUUID();
    const suffix = token.replaceAll('-', '');
    const id = `org.alef.e2e.native.${suffix}`;
    const scheme = `alefe2e${suffix}`;
    const targets = { platform: process.platform, startupUrl: `${scheme}://startup/queued`, secondUrl: `${scheme}://second/trailing?run=${suffix}`, cwd: process.cwd() };
    const site = prepareSite(`${kind}-${suffix}`, `modules/app/${kind}`, { replacements: { ID: id, SCHEME: scheme }, targets });
    const j = identity(id, scheme, site, exe, kind);
    const journal = join(journalRoot, `${token}.json`);
    const children = [];
    const problems = [];
    let cleanupOk = false;
    let journalWritten = false;
    try {
      // Absence preflight and journal both precede any native mutation.
      verify(j, false);
      if (kind === 'deeplink' && process.platform === 'linux') {
        expect(command('xdg-mime', ['query', 'default', `x-scheme-handler/${scheme}`]) === '', 'unique scheme already has a foreign default; refusing mutation');
      }
      writeFileSync(journal, JSON.stringify(j), { flag: 'wx' });
      journalWritten = true;
      const first = startApp({ exe, verbose, env: { ALEF_E2E_APP_INTEGRATION: '1', ALEF_E2E_RELEASE: join(site, 'runner-release.json') }, args: ['--app', site, ...(kind === 'deeplink' ? ['--', '--role=first', 'first.txt', targets.startupUrl] : [])] });
      children.push(first);
      await first.waitFor(line => line.includes('ALEF_E2E integration native-ready'), wait, 'native enable checkpoint');
      verify(j, true);
      writeFileSync(join(site, 'runner-release.json'), JSON.stringify({ verified: true }));
      if (kind === 'deeplink') {
        await first.waitFor(line => line.includes('ALEF_E2E integration first-ready'), wait, 'single-instance endpoint');
        const second = startApp({ exe, verbose, env: { ALEF_E2E_APP_INTEGRATION: '1', ALEF_E2E_RELEASE: join(site, 'runner-release.json') }, args: ['--app', site, targets.secondUrl, '--', '--role=second', 'other.txt'] });
        children.push(second);
        await second.waitFor(line => line.includes('ALEF_E2E RESULT'), wait, 'second verdict', { survivesExit: true });
        expect((await second.waitForExit(bound))?.code === 0, 'second did not quit cleanly');
        expect(await second.waitForClosed(5000), 'second pipes did not close');
        checkPage(second, ['later-instance-false'], 'second');
      }
      await first.waitFor(line => line.includes('ALEF_E2E RESULT'), wait, 'first verdict', { survivesExit: true });
      const checks = kind === 'autostart' ? ['autostart-enable', 'autostart-disable'] : ['startup-queued-after-unrelated-subscription', 'registration', 'first-instance-true', 'second-url-and-args-separated-no-duplicates', 'unregistration'];
      verify(j, false);
      expect((await first.waitForExit(bound))?.code === 0, 'first did not quit cleanly');
      expect(await first.waitForClosed(5000), 'first pipes did not close');
      checkPage(first, checks, 'first');
    } catch (error) {
      problems.push(error.message);
    } finally {
      let childrenStopped = true;
      for (const child of children.reverse()) {
        try { await stop(child); } catch (error) { childrenStopped = false; problems.push(`child cleanup: ${error.message}`); }
      }
      if (journalWritten && childrenStopped) {
        try { cleanup(j); cleanupOk = true; } catch (error) { problems.push(`native cleanup: ${error.message}`); }
        if (cleanupOk) rmSync(journal, { force: true });
      }
      console.log(`    cleanup: owned children ${childrenStopped ? 'stopped/awaited' : 'stop failed'}; native entries ${cleanupOk ? 'removed; journal retired' : 'retained in owned journal for recovery'}`);
    }
    return { problems, lines: children.flatMap(child => child.lines) };
  }
  return { autostart: () => run('autostart'), deeplink: () => run('deeplink') };
}
