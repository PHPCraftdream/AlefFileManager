// SPDX-License-Identifier: MIT OR Apache-2.0
// Scan release binaries without decoding or modifying their bytes.
import { readFileSync, statSync } from 'node:fs';

const looked_for = ['e2e.dropFiles', 'e2e.once', 'e2e.flood', 'ALEF_E2E', 'ALEF_SPIKE_ORIGIN'];
const encodings = [['UTF8', 'utf8'], ['UTF16LE', 'utf16le']];

function check_binary() {
  const args = process.argv.slice(2);
  if (args.length !== 1 || !args[0] || args[0].startsWith('-') || args[0].includes('\0')) {
    console.error('Usage: node scripts/supply/check-no-hooks.mjs <binary-path>');
    return 1;
  }

  let bytes;
  try {
    if (!statSync(args[0]).isFile()) {
      console.error('Cannot read binary: expected a regular file.');
      return 1;
    }
    bytes = readFileSync(args[0]);
  } catch (error) {
    const code = typeof error.code === 'string' ? error.code.slice(0, 64) : 'READ_ERROR';
    console.error(`Cannot read binary: ${code}`);
    return 1;
  }

  console.log(`Looked for: ${looked_for.join(', ')}`);
  console.log(`Byte size: ${bytes.length}`);
  let found = false;
  for (const marker of looked_for) {
    for (const [label, encoding] of encodings) {
      if (bytes.includes(Buffer.from(marker, encoding))) {
        console.error(`Found: ${marker} (${label})`);
        found = true;
      }
    }
  }
  return found ? 1 : 0;
}

process.exitCode = check_binary();
