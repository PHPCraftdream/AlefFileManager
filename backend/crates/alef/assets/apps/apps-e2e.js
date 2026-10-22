// The apps page automation runs only in a feature=e2e binary with ALEF_E2E=1.
import { call, ready, settled } from './apps.js';
const emit = data => call('e2e.report', { line: JSON.stringify(data) });
const need = (node, name) => { if (!node) throw new Error(`missing UI control: ${name}`); return node; };
try {
  await ready;
  await settled;
  const ui = await import('./apps.js');
  const steps = await call('runtime.apps.automation');
  if (!Array.isArray(steps)) throw new Error('ALEF_E2E_APPS_UI must be a JSON step array');
  for (const step of steps) {
    if (step.search !== undefined) {
      const search = need(document.querySelector('input[type=search]'), 'search');
      search.value = step.search; search.dispatchEvent(new Event('input', { bubbles: true }));
    }
    if (step.select) {
      const button = [...document.querySelectorAll('.app')].find(node => node.textContent.includes(step.select));
      need(button, `app ${step.select}`).click(); await ui.settled;
    }
    if (step.choose) {
      const row = [...document.querySelectorAll('.right')].find(node => node.querySelector('.what')?.textContent === step.choose);
      const radio = need(row?.querySelector(`input[value="${step.decision}"]`), `${step.choose} ${step.decision}`);
      radio.checked = true; radio.dispatchEvent(new Event('change', { bubbles: true }));
    }
    if (step.confirm) {
      const row = [...document.querySelectorAll('.right')].find(node => node.querySelector('.what')?.textContent === step.confirm);
      const check = need(row?.querySelector('input[type=checkbox]'), `confirm ${step.confirm}`);
      check.checked = true; check.dispatchEvent(new Event('change', { bubbles: true }));
    }
    if (step.save) {
      const row = [...document.querySelectorAll('.right')].find(node => node.querySelector('.what')?.textContent === step.save);
      need(row?.querySelector('button'), `save ${step.save}`).click(); await ui.settled;
    }
    if (step.reset) {
      const button = [...document.querySelectorAll('button')].find(node => node.textContent === 'Reset decisions');
      need(button, 'reset').click(); await ui.settled;
    }
    if (step.negativeRed) {
      try {
        await call('runtime.apps.set', { identity: step.identity ?? ui.view?.selected()?.identity, right: step.negativeRed, decision: 'allow', confirm_red: false });
        await emit({ type: 'check', name: 'red-confirmation-required', ok: false, error: 'the backend accepted an unconfirmed red right' });
      } catch (error) {
        const text = error instanceof Error ? error.message : String(error);
        await emit({ type: 'check', name: 'red-confirmation-required', ok: text.includes('confirm-red'), error: text });
      }
    }
    if (step.snapshot) {
      await emit({
        type: 'snapshot',
        name: step.snapshot,
        selected: ui.view?.selected()?.identity,
        list: ui.view?.visible().map(app => ({ id: app.id, name: app.name, version: app.version, source: app.source, availability: app.availability, summary: app.summary })),
        rows: [...document.querySelectorAll('.right')].map(row => ({
          right: row.querySelector('.what')?.textContent,
          checked: row.querySelector('input[type=radio]:checked')?.value ?? null,
          red: row.classList.contains('red'),
          confirmed: Boolean(row.querySelector('input[type=checkbox]')?.checked),
          error: row.querySelector('.error')?.textContent ?? '',
        })),
      });
    }
    if (step.close) await call('runtime.apps.close');
  }
} catch (error) { await emit({ type: 'failure', message: error?.message ?? String(error) }); }
