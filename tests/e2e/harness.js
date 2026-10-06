// SPDX-License-Identifier: MIT OR Apache-2.0
// Browser-side helpers shared by the scenario pages. The runner copies this file next to each page
// together with the transpiled @alef-tron/api (./src), and the runtime started with ALEF_E2E=1
// prints what a page reports through `e2e.report` as `ALEF_E2E <line>` on stderr.
import * as api from './src/index.js';

export { api };
export const MIB = 1024 * 1024;
export const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
export const same = (left, right) => left.length === right.length && left.every((byte, index) => byte === right[index]);

export function pattern(size) {
  const bytes = new Uint8Array(size);
  for (let i = 0; i < size; i += 1) bytes[i] = (i * 31 + (i >> 8)) & 255;
  return bytes;
}

export async function until(predicate, ms, what) {
  const deadline = performance.now() + ms;
  while (!predicate()) {
    if (performance.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(25);
  }
}

/** Sends one line to the runtime log; a page that cannot reach the runtime says so on the console. */
export async function report(line) {
  try {
    await api.call('e2e.report', { line });
  } catch (error) {
    console.error(`ALEF_E2E-page could not report "${line}": ${error?.message ?? error}`);
  }
}

/** The error a promise rejects with, or `null` when it resolves. */
export const rejection = promise => promise.then(() => null, error => error);

/** Collects named checks; each outcome is reported as `check <name> ok|FAILED ...`. */
export function suite() {
  const results = [];
  async function check(name, body) {
    const started = performance.now();
    try {
      const detail = await body();
      results.push({ name, ok: true });
      await report(`check ${name} ok ${Math.round(performance.now() - started)}ms ${detail ?? ''}`);
    } catch (error) {
      results.push({ name, ok: false });
      console.error(`ALEF_E2E-page check ${name} FAILED ${error?.message ?? error}`);
      await report(`check ${name} FAILED ${error?.message ?? error}`);
    }
  }
  const failed = () => results.filter(item => !item.ok).map(item => item.name);
  return { check, failed };
}

export const verdict = failed => report(`RESULT ${failed.length === 0 ? 'PASS' : 'FAIL'} ${failed.join(',')}`);

/** Runs a scenario with a watchdog; an exception becomes a FAIL verdict and a console line. */
export function guard(main) {
  setTimeout(() => report('RESULT FAIL timeout'), 120000);
  main().catch(error => {
    console.error(`ALEF_E2E-page fatal ${error?.message ?? error}`);
    return report(`RESULT FAIL ${error?.message ?? error}`);
  });
}
