#!/usr/bin/env node
// SPDX-License-Identifier: MIT OR Apache-2.0
import { cli } from '../src/cli.mjs';

try {
  const [major, minor] = process.versions.node.split('.').map(Number);
  if (major < 22 || (major === 22 && minor < 12)) throw new Error('create-alef-runtime requires Node >=22.12.0.');
  await cli();
} catch (error) {
  console.error(`create-alef-runtime: ${error.message}`);
  process.exitCode = 1;
}
