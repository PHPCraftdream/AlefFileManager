// SPDX-License-Identifier: MIT OR Apache-2.0
import { lstat, open } from 'node:fs/promises';
import { constants } from 'node:fs';
import path from 'node:path';
import { validateName } from './zip.mjs';
import { noSymlinks } from './keys.mjs';

const LIMIT = 1024 * 1024;
const fail = message => { throw new Error(`alef.ktav: ${message}`); };
/** Basic text hygiene only; full runtime manifest validation is not performed. */
export async function readManifest(dir = process.cwd()) {
  const file = path.resolve(dir, 'alef.ktav');
  try {
    await noSymlinks(file);
    const before = await lstat(file);
    if (!before.isFile()) fail('must be a regular file');
    if (before.size > LIMIT) fail('exceeds basic check size limit (1 MiB)');
    const handle = await open(file, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
    let bytes;
    try {
      const stat = await handle.stat();
      if (!stat.isFile() || stat.size !== before.size || stat.dev !== before.dev || stat.ino !== before.ino) fail('file changed');
      bytes = Buffer.alloc(stat.size + 1); let size = 0;
      while (size < bytes.length) {
        const { bytesRead } = await handle.read(bytes, size, bytes.length - size, size);
        if (!bytesRead) break;
        size += bytesRead;
      }
      if (size !== stat.size) fail('file changed');
      bytes = bytes.subarray(0, size);
    } finally { await handle.close(); }
    let text;
    try { text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes); }
    catch { fail('invalid UTF-8'); }
    if (text.startsWith('\ufeff')) fail('BOM is forbidden');
    if (text.includes('\t')) fail('tabs are forbidden');
    if (text.trim().length === 0) fail('must be nonempty');
    return Object.freeze({ file, bytes, assets: readAssets(text) });
  } catch (error) {
    if (error.message.startsWith('alef.ktav:')) throw error;
    throw new Error(`alef.ktav: ${error.message}`, { cause: error });
  }
}

// Not a schema parser: a small conservative subset of the runtime's Ktav
// examples (plain/raw/quoted keys and scalars, multiline ((...)), containers).
function readAssets(text) {
  let depth = 0; let found; let i = 0;
  const rootDepth = text.replace(/^[ \r\n]*(?:##[^\n]*(?:\n|$)[ \r\n]*)*/, '').startsWith('{') ? 1 : 0;
  while (i < text.length) {
    if (text.startsWith('##', i)) { i = text.indexOf('\n', i); if (i < 0) break; continue; }
    if (text.startsWith('((', i)) {
      const end = text.indexOf('))', i + 2);
      if (end < 0) { if (/assets/i.test(text.slice(i))) fail('assets: unsupported encoding'); break; }
      let after = end + 2;
      while (after < text.length && /\s/.test(text[after])) after++;
      if (depth === rootDepth && text[after] === ':') fail('assets: multiline keys unsupported by basic reader');
      i = end + 2; continue;
    }
    const start = i; let token;
    if (/['"`]/.test(text[i])) {
      const quote = text[i++]; const begin = i;
      while (i < text.length && text[i] !== quote) { if (text[i] === '\\') i++; i++; }
      token = text.slice(begin, i); i++;
      if (token.includes('\\')) {
        let at = i; while (/[ \r]/.test(text[at] ?? '') && at < text.length) at++;
        if (text[at] === ':' && depth === rootDepth) fail('assets: escaped keys unsupported by basic reader');
      }
    } else if (/[A-Za-z0-9_.-]/.test(text[i])) {
      while (i < text.length && /[A-Za-z0-9_.-]/.test(text[i])) i++;
      token = text.slice(start, i);
    } else {
      if ('{['.includes(text[i])) depth++;
      if ('}]'.includes(text[i])) depth--;
      i++; continue;
    }
    let at = i; while (at < text.length && /\s/.test(text[at])) at++;
    if (token !== 'assets' || text[at] !== ':' || depth !== rootDepth) continue;
    if (found !== undefined) fail('assets: duplicate field');
    at++; const raw = text[at] === ':'; if (raw) at++;
    while (at < text.length && /[ \r]/.test(text[at])) at++;
    let value; let end;
    if (text.startsWith('((\n', at) || text.startsWith('((\r\n', at)) {
      const begin = text.indexOf('\n', at) + 1; end = text.indexOf('))', begin);
      if (end < 0) fail('assets: unsupported multiline encoding');
      value = text.slice(begin, end).replace(/\r?\n$/, ''); end += 2;
    } else if (/['"`]/.test(text[at] ?? '') && !raw) {
      const quote = text[at]; end = text.indexOf(quote, at + 1);
      if (end < 0) fail('assets: unsupported quoted encoding');
      value = text.slice(at + 1, end); end++;
      if (/[\\\r\n]/.test(value)) fail('assets: unsupported quoted encoding');
    } else {
      end = text.indexOf('\n', at); if (end < 0) end = text.length;
      value = text.slice(at, end).replace(/\r$/, '').trim();
      if (!raw && /^(null|true|false|[+-]?\d+(?:\.\d+)?)$/.test(value)) fail('assets: must be a string');
    }
    const newline = text.indexOf('\n', end);
    const tail = text.slice(end, newline < 0 ? text.length : newline).trim();
    if (tail && !tail.startsWith('##')) fail('assets: ambiguous encoding');
    try { validateName(value); } catch (error) { fail(`assets: ${error.message}`); }
    if (/^(meta-inf|alef\.ktav|icon\.png)(\/|$)/i.test(value)) fail('assets: reserved control/root file collision');
    found = value; i = end;
  }
  return found ?? 'dist';
}

export async function check(dir = process.cwd()) {
  const { file } = await readManifest(dir);
  return Object.freeze({ file, message: 'Basic check passed; full runtime manifest validation not performed.' });
}
