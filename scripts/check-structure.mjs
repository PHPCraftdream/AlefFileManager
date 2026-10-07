// Project structure limits for our own code (frontend and backend).
import { readdirSync, readFileSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const MAX_ENTRIES = 7;
const MAX_LINES = 700;
const SOURCE_ROOTS = ['frontend/src', 'frontend/test', 'backend/crates', 'scripts', 'packages', 'tests', 'experiments'];
const IGNORED = new Set(['node_modules', 'dist', 'target']);
const CODE = /\.(rs|ts|tsx|js|jsx|mjs|cjs|css|html)$/;

const root = fileURLToPath(new URL('../', import.meta.url));
const violations = [];

function lineCount(file) {
  const text = readFileSync(file, 'utf8');
  const lines = text.split(/\r?\n/);
  return lines.at(-1) === '' ? lines.length - 1 : lines.length;
}

function walk(directory) {
  const entries = readdirSync(directory, { withFileTypes: true })
    .filter(entry => !IGNORED.has(entry.name));
  const relative = path.relative(root, directory).replaceAll('\\', '/');
  if (entries.length > MAX_ENTRIES) {
    violations.push(`${relative}/: ${entries.length} entries (max ${MAX_ENTRIES})`);
  }
  for (const entry of entries) {
    const full = path.join(directory, entry.name);
    if (entry.isDirectory()) walk(full);
    else if (CODE.test(entry.name)) {
      const lines = lineCount(full);
      if (lines > MAX_LINES) violations.push(`${relative}/${entry.name}: ${lines} lines (max ${MAX_LINES})`);
    }
  }
}

for (const source of SOURCE_ROOTS) {
  const directory = path.join(root, source);
  if (existsSync(directory)) walk(directory);
}

if (violations.length > 0) {
  console.error(`Structure limits violated:\n${violations.map(line => `  ${line}`).join('\n')}`);
  process.exit(1);
}
console.log(`Structure OK: <= ${MAX_ENTRIES} entries per directory, <= ${MAX_LINES} lines per file.`);
