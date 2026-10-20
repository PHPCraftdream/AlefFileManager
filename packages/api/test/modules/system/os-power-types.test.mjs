// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { API } from 'typescript/unstable/sync';

test('the_power_overloads_type_check_and_mismatched_handlers_and_unknown_events_are_refused', () => {
  const file = fileURLToPath(new URL('./os-power.consumer.ts', import.meta.url));
  const source = `
    import { os } from '../../../src/system/os.ts';
    import type { Theme } from '../../../types/index.ts';
    async function consume() {
      let seen: Theme | undefined;
      await os.on('theme-changed', (theme: Theme) => { seen = theme; });
      await os.on('suspend', () => {});
      await os.on('resume', () => {});
      await os.on('theme-changed', () => {});
      // @ts-expect-error power handlers take no argument
      await os.on('suspend', (why: number) => { void why; });
      // @ts-expect-error unknown os event
      await os.on('bogus', () => {});
    }
    void consume;
  `;
  const config = fileURLToPath(new URL('./os-power.consumer.tsconfig.json', import.meta.url));
  const virtual = new Map([
    [file.replaceAll('\\', '/'), source],
    [config.replaceAll('\\', '/'), JSON.stringify({
      compilerOptions: {
        target: 'ES2022', module: 'ESNext', moduleResolution: 'Bundler',
        strict: true, noEmit: true, skipLibCheck: true, allowImportingTsExtensions: true,
      },
      files: [file],
    })],
  ]);
  const api = new API({
    cwd: process.cwd(),
    fs: {
      readFile: key => virtual.get(key.replaceAll('\\', '/')),
      fileExists: key => virtual.has(key.replaceAll('\\', '/')) || undefined,
    },
  });
  try {
    const snapshot = api.updateSnapshot({ openProjects: [config] });
    try {
      const program = snapshot.getProject(config).program;
      const diagnostics = [
        ...program.getProgramDiagnostics(), ...program.getGlobalDiagnostics(),
        ...program.getSyntacticDiagnostics(), ...program.getSemanticDiagnostics(),
      ];
      assert.deepEqual(diagnostics, []);
    } finally {
      snapshot.dispose();
    }
  } finally {
    api.close();
  }
});
