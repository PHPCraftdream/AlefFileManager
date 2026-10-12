#!/usr/bin/env node
// SPDX-License-Identifier: MIT OR Apache-2.0
import { pack, verify, check, createKey, listKeys, build, dev } from '../src/index.mjs';

const help = `alef-tron keys [name] | keys --list
alef-tron pack <dir> [--key <name> | --unsigned] [--out <file>] [--include <names>]
alef-tron verify <file>
alef-tron check [dir]
alef-tron build [dir] [--out <dir>]
alef-tron dev [dir] [--ready-timeout <ms>] [-- <runtime args>]
--include: repeat or use commas for .git,node_modules,.alef-tron;
key home and .alef-tron are always excluded.
check: basic text hygiene only; full runtime manifest validation is not performed.
build: runs the configured build command and assembles an app directory;
dev: runs the dev command, waits for alef-tron.devUrl, then the runtime.
`;
const fail = message => { throw new Error(message); };
function argumentsFor(args, allowed, repeat = new Set()) {
  const positional = []; const options = {};
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (!arg) fail('Empty positional argument');
    if (!arg.startsWith('-')) { positional.push(arg); continue; }
    if (!arg.startsWith('--')) fail(`Unknown option: ${arg}`);
    const name = arg.slice(2);
    if (!Object.hasOwn(allowed, name)) fail(`Unknown option: ${arg}`);
    if (Object.hasOwn(options, name) && !repeat.has(name)) fail(`Repeated option: ${arg}`);
    let value = true;
    if (allowed[name]) {
      value = args[++i];
      if (!value || value.startsWith('-')) fail(`Missing value: ${arg}`);
    }
    if (repeat.has(name)) (options[name] ??= []).push(value);
    else options[name] = value;
  }
  return { positional, options };
}
async function main(args) {
  const command = args.shift();
  if (command === '--help' && args.length === 0) { process.stdout.write(help); return; }
  if (command === 'keys') {
    const { positional, options } = argumentsFor(args, { list: false });
    if (positional.length > 1 || (options.list && positional.length)) fail('Usage: keys [name] | keys --list');
    if (options.list) {
      for (const key of await listKeys()) process.stdout.write(`${key.name} ${key.fingerprint}\n`);
    } else {
      const key = await createKey(positional[0]);
      process.stdout.write(`${key.name} ${key.fingerprint}\nkey ${Buffer.from(key.key).toString('base64')}\n`);
    }
    return;
  }
  if (command === 'pack') {
    const { positional, options } = argumentsFor(args, { key: true, unsigned: false, out: true, include: true }, new Set(['include']));
    if (positional.length !== 1) fail('Usage: pack <dir> [options]');
    if (options.include) {
      options.include = options.include.flatMap(value => value.split(','));
      if (new Set(options.include).size !== options.include.length || options.include.some(name => !['.git', 'node_modules', '.alef-tron'].includes(name))) fail('Invalid or repeated --include basename');
    }
    const result = await pack(positional[0], options);
    process.stdout.write(`${result.out}\n`); return;
  }
  if (command === 'verify' || command === 'check') {
    const { positional } = argumentsFor(args, {});
    if (positional.length > 1 || (command === 'verify' && positional.length !== 1)) fail(`Usage: ${command} ${command === 'verify' ? '<file>' : '[dir]'}`);
    if (command === 'verify') {
      const result = await verify(positional[0]);
      process.stdout.write(result.status === 'Signed' ? `signed by ${result.fingerprint}\n` : 'unsigned\n');
    } else process.stdout.write(`${(await check(positional[0])).message}\n`);
    return;
  }
  if (command === 'build') {
    const { positional, options } = argumentsFor(args, { out: true });
    if (positional.length > 1) fail('Usage: build [dir] [--out <dir>]');
    const result = await build(positional[0], { out: options.out, stdio: 'inherit' });
    process.stdout.write(`${result.dir}\n`); return;
  }
  if (command === 'dev') {
    const separator = args.indexOf('--');
    const appArgs = separator === -1 ? [] : args.splice(separator).slice(1);
    const { positional, options } = argumentsFor(args, { 'ready-timeout': true });
    if (positional.length > 1) fail('Usage: dev [dir] [--ready-timeout <ms>] [-- <runtime args>]');
    const devOptions = { stdio: 'inherit', appArgs };
    if (options['ready-timeout'] !== undefined) {
      devOptions.readyTimeoutMs = Number(options['ready-timeout']);
      if (!Number.isSafeInteger(devOptions.readyTimeoutMs) || devOptions.readyTimeoutMs < 1) fail('--ready-timeout must be a positive integer');
    }
    const result = await dev(positional[0], devOptions);
    if (typeof result.code === 'number') process.exitCode = result.code; // null must not overwrite a signal exit code
    return;
  }
  fail('Unknown command; use --help');
}
try { await main(process.argv.slice(2)); }
catch (error) { process.stderr.write(`alef-tron: ${error.message.replace(/-----BEGIN [\s\S]*/g, '[redacted private material]')}\n`); process.exitCode = 1; }
