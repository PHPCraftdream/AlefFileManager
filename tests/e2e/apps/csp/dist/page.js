// External-resource scenario: the CSP built from the manifest's `external` section decides whether a
// page may reach another origin (docs/stages/m1-core.md: an empty `external.connect` blocks an
// external fetch, a listed address passes). The runner serves `origin` and counts the requests that
// actually arrive, which is the evidence independent of what this page reports.
import { api, guard, sleep, suite, verdict } from './harness.js';

async function main() {
  const { origin, expect } = await (await fetch('targets.json')).json();
  const { check, failed } = suite();

  await check(`external-fetch-is-${expect === 'open' ? 'allowed' : 'blocked'}`, async () => {
    const outcome = await fetch(`${origin}/ping`).then(
      response => `status ${response.status}`,
      error => `rejected ${error?.name ?? 'error'}`,
    );
    if (expect === 'open' && outcome !== 'status 200') throw new Error(`a listed origin was refused: ${outcome}`);
    if (expect === 'closed' && !outcome.startsWith('rejected')) throw new Error(`an unlisted origin was reachable: ${outcome}`);
    return outcome;
  });
  await check('inline-script-is-blocked', async () => {
    window.alefInlineScriptRan = false;
    const script = document.createElement('script');
    script.textContent = 'window.alefInlineScriptRan = true;';
    document.head.append(script);
    await sleep(100);
    if (window.alefInlineScriptRan) throw new Error('an inline script ran although script-src has no unsafe-inline');
  });
  await check('the-transport-still-works-under-the-policy', async () => {
    const echoed = await api.call('e2e.echo', { under: 'csp' });
    if (echoed.under !== 'csp') throw new Error('the echo failed');
  });
  await verdict(failed());
}

guard(main);
