// SPDX-License-Identifier: MIT OR Apache-2.0
import { lstat, open } from 'node:fs/promises';
import { constants } from 'node:fs';
import path from 'node:path';
import { noSymlinks } from './keys.mjs';

const LIMIT = 1024 * 1024;
const fail = message => { throw new Error(`alef.ktav: ${message}`); };
/** Basic text hygiene only; full runtime manifest validation is not performed. */
export async function check(dir = process.cwd()) {
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
    return Object.freeze({ file, message: 'Basic check passed; full runtime manifest validation not performed.' });
  } catch (error) {
    if (error.message.startsWith('alef.ktav:')) throw error;
    throw new Error(`alef.ktav: ${error.message}`, { cause: error });
  }
}
