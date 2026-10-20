// SPDX-License-Identifier: MIT OR Apache-2.0
import { api, guard, report, suite, verdict } from './harness.js';

const checks = suite();
const marker = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
const assert = (condition, message) => { if (!condition) throw new Error(message); };

async function main() {
  await report(`log final-marker ${marker}`);
  await report(`path appData ${await api.path.appData()}`);
  await checks.check('log-crashes-start-empty-and-invalid-crash-id-is-rejected', async () => {
    const crashes = await api.log.crashes();
    assert(Array.isArray(crashes) && crashes.length === 0, `expected no initial crashes, got ${crashes.length}`);
    const error = await api.log.crash('../bad').then(() => null, error => error);
    assert(error?.code === 'INVALID_ARGUMENT', `expected INVALID_ARGUMENT, got ${error?.code}`);
  });
  await checks.check('log-level-filter-redaction-and-newline-message-are-exact', async () => {
    await api.log.info('line-one\nline-two', { password: 'e2e-sensitive-password', marker });
    const own = (await api.log.read({ limit: 100 })).find(record => record.message === 'line-one\nline-two');
    assert(own?.level === 'info' && own.fields.password === '[redacted]' && own.fields.marker === marker, 'log info or redaction differs');
    for (const level of ['debug', 'warn', 'error']) await api.log[level](`filter-${level}-${marker}`);
    for (const level of ['debug', 'info', 'warn', 'error']) {
      const records = await api.log.read({ level, limit: 100 });
      assert(records.every(record => record.level === level), `${level} filter leaked another level`);
      if (level !== 'info') assert(records.some(record => record.message === `filter-${level}-${marker}`), `${level} record missing`);
    }
  });
  await checks.check('log-rotation-retains-newest-marker', async () => {
    for (let index = 0; index < 80; index += 1) {
      await api.log.info(`rotate-${index}-${marker}-${'x'.repeat(8192)}`);
      await new Promise(resolve => setTimeout(resolve, 6));
    }
    const records = await api.log.read({ limit: 100 });
    assert(records.some(record => record.message.startsWith(`rotate-79-${marker}-`)), 'newest marker missing after rotation');
    await report(`log final-marker ${marker}`);
  });
  await verdict(checks.failed());
  await api.app.quit(0);
}

guard(main);
