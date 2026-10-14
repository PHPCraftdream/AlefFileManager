// SPDX-License-Identifier: MIT OR Apache-2.0
// The `fm-package` scenario: the built File Manager is packed like any Alef application and run as
// one. A throw-away key signs the package via the tools API, `alef run --trust-key` accepts it, and
// the runtime-side ALEF_READY line names the application. Jobs without a built frontend/app (the
// File Manager frontend is built only on Linux in CI) skip with a clear reason instead of failing.
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import os from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { startApp } from '../../lib.mjs';
import { createKey, pack, verify } from '../../../../packages/tools/src/index.mjs';

const HERE = dirnameOf();
function dirnameOf() {
  return fileURLToPath(new URL('.', import.meta.url));
}

export function fmPackageScenarios({ exe, verbose, timeoutMs }) {
  return {
    'fm-package': async () => {
      const problems = [];
      const expect = (what, condition, detail = '') => {
        console.log(`    ${condition ? 'ok  ' : 'FAIL'} ${what}${condition ? '' : `: ${detail}`}`);
        if (!condition) problems.push(`${what}${detail ? `: ${detail}` : ''}`);
      };
      const app = resolve(HERE, '..', '..', '..', '..', 'frontend', 'app');
      if (!existsSync(join(app, 'alef.ktav'))) {
        console.log('    skip: frontend/app is not built in this job (the File Manager frontend is built only on Linux); nothing was exercised');
        return { problems, lines: [] };
      }
      const scratchDir = mkdtempSync(join(os.tmpdir(), 'alef-e2e-fm-package-'));
      try {
        const keyHome = join(scratchDir, 'keys');
        const key = await createKey('e2e', { home: keyHome });
        const pkg = join(scratchDir, 'file-manager.alef');
        await pack(app, { key: 'e2e', home: keyHome, out: pkg });
        const opened = await verify(pkg);
        expect('the_packaged_file_manager_verifies_with_the_signing_key', opened.status === 'Signed' && opened.fingerprint === key.fingerprint,
          `status ${opened.status}, fingerprint ${opened.fingerprint}`);
        const home = join(scratchDir, 'home');
        const running = startApp({ exe, verbose, args: ['run', pkg, '--trust-key', key.fingerprint], env: { ALEF_HOME: home, ALEF_APP_HOME: home } });
        try {
          const ready = await running.waitFor(line => line.startsWith('ALEF_READY app=org.alef.filemanager '), timeoutMs, 'ALEF_READY of the File Manager');
          expect('the_packed_file_manager_reports_ready_under_its_own_id', ready.includes('app=org.alef.filemanager'), ready);
        } finally {
          running.stop();
          const exit = await running.waitForExit(30000);
          expect('the_packed_file_manager_exits_when_stopped', exit !== null && exit !== undefined, 'still running after 30 s');
        }
      } finally {
        rmSync(scratchDir, { recursive: true, force: true });
      }
      return { problems, lines: [] };
    },
  };
}
