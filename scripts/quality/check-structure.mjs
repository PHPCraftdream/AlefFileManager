// Project structure limits for our own code (frontend and backend).
import { readdirSync, readFileSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const MAX_ENTRIES = 7;
export const MAX_LINES = 1000;
export const SOURCE_ROOTS = ['frontend/src', 'frontend/test', 'backend/crates', 'scripts', 'packages', 'tests', 'experiments'];
const IGNORED = new Set(['node_modules', 'dist', 'target']);
const CODE = /\.(rs|ts|tsx|js|jsx|mjs|cjs|css|html)$/;

function lineCount(file) {
  const text = readFileSync(file, 'utf8');
  const lines = text.split('\n');
  return lines.at(-1) === '' ? lines.length - 1 : lines.length;
}

/** Returns the violations (strings) of the limits below `root` for the given source roots. */
export function checkStructure(root, { sources = SOURCE_ROOTS, maxEntries = MAX_ENTRIES, maxLines = MAX_LINES } = {}) {
  const violations = [];
  function walk(directory) {
    const entries = readdirSync(directory, { withFileTypes: true })
      .filter(entry => !IGNORED.has(entry.name));
    const relative = path.relative(root, directory).replaceAll('\\', '/');
    if (entries.length > maxEntries) {
      violations.push(`${relative}/: ${entries.length} entries (max ${maxEntries})`);
    }
    for (const entry of entries) {
      const full = path.join(directory, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (CODE.test(entry.name)) {
        const lines = lineCount(full);
        if (lines > maxLines) violations.push(`${relative}/${entry.name}: ${lines} lines (max ${maxLines})`);
      }
    }
  }
  for (const source of sources) {
    const directory = path.join(root, source);
    if (existsSync(directory)) walk(directory);
  }
  return violations;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const violations = checkStructure(fileURLToPath(new URL('../../', import.meta.url)));
  if (violations.length > 0) {
    console.error(`Structure limits violated:\n${violations.map(line => `  ${line}`).join('\n')}`);
    process.exit(1);
  }
  console.log(`Structure OK: <= ${MAX_ENTRIES} entries per directory, <= ${MAX_LINES} lines per file.`);
}
