// SPDX-License-Identifier: MIT OR Apache-2.0
import { createInterface } from 'node:readline';
import { generate } from './generate.mjs';
import { validateOptions, validateDirectory, packageName } from './validate.mjs';

export function parseArgs(args) {
  const options = {};
  for (let index = 0; index < args.length; index++) {
    const arg = args[index];
    if (arg === '--yes') {
      if (options.yes) throw new Error('Duplicate --yes.');
      options.yes = true;
    } else if (['--name', '--id', '--bundler'].includes(arg)) {
      const key = arg.slice(2);
      if (Object.hasOwn(options, key)) throw new Error(`Duplicate ${arg}.`);
      const value = args[++index];
      if (value === undefined || value.startsWith('--')) throw new Error(`Missing value for ${arg}.`);
      options[key] = value;
    } else if (arg.startsWith('-')) throw new Error(`Unknown option: ${arg}`);
    else {
      if (Object.hasOwn(options, 'directory')) throw new Error('Only one directory may be supplied.');
      options.directory = arg;
    }
  }
  return options;
}

// Keep every line, including lines delivered before the next question. readline.question
// alone drops those in piped scripts. Each question has its own two-minute deadline.
export function prompts(input, output, timeoutMs = 120_000) {
  const lines = [];
  let closed = false;
  let waiting;
  const reader = createInterface({ input, crlfDelay: Infinity, terminal: false });
  reader.on('line', line => {
    if (waiting) { const resolve = waiting; waiting = undefined; resolve(line); }
    else lines.push(line);
  });
  reader.on('close', () => {
    closed = true;
    if (waiting) { const resolve = waiting; waiting = undefined; resolve(undefined); }
  });
  return {
    async ask(label, fallback) {
      output.write(`${label} [${fallback}]: `);
      let line;
      if (lines.length) line = lines.shift();
      else if (!closed) {
        line = await new Promise((resolve, reject) => {
          const timer = setTimeout(() => {
            waiting = undefined;
            reject(new Error(`Prompt timed out after ${timeoutMs / 1000}s: ${label}. Use --yes for defaults.`));
          }, timeoutMs);
          waiting = value => { clearTimeout(timer); resolve(value); };
        });
      }
      if (line === undefined) throw new Error(`Input ended (EOF) while asking for ${label}. Use --yes for defaults.`);
      return line === '' ? fallback : line;
    },
    close() { reader.close(); },
  };
}

export async function cli(args = process.argv.slice(2), { input = process.stdin, output = process.stdout, cwd = process.cwd(), timeoutMs } = {}) {
  const options = parseArgs(args);
  if (!options.yes) {
    const reader = prompts(input, output, timeoutMs);
    try {
      options.directory ??= await reader.ask('Directory', 'my-alef-app');
      validateDirectory(options.directory);
      packageName(options.directory);
      const defaults = validateOptions({ directory: options.directory });
      options.name ??= await reader.ask('Display name', defaults.name);
      options.id ??= await reader.ask('Application ID', defaults.id);
      options.bundler ??= await reader.ask('Bundler (rsbuild|vite)', 'rsbuild');
    } finally { reader.close(); }
  } else options.directory ??= 'my-alef-app'; // Do not construct a reader or touch input.
  const result = await generate(options, cwd);
  output.write(`\nCreated ${result.target}\n\nNext steps:\n  cd "${result.directoryName}"\n  npm install\n  npm run dev\n  npm run build\n  npx alef-runtime pack\n`);
  return result;
}
