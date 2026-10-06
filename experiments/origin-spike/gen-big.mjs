// Generates the synthetic 2 MiB asset big.js (not stored in git).
import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const header = '// Synthetic 2 MiB asset for the M0.1 origin spike. ';
const body = '0123456789abcdef'.repeat(Math.ceil((2 * 1024 * 1024) / 16));
writeFileSync(
  fileURLToPath(new URL('./big.js', import.meta.url)),
  `${(header + body).slice(0, 2 * 1024 * 1024)}\n`,
);
