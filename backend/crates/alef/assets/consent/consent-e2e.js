// End-to-end runs only (`ALEF_E2E=1`): the runtime writes this file next to the page when the run asks
// for it. Plays the clicks of a user from a script and reports what the page shows, so that the page
// is tested through its own controls and not through a shortcut.
const same = (a, b) => a === b;

function rowOf(right) {
  const row = [...document.querySelectorAll('.right')].find(item => same(item.dataset.right, right));
  if (!row) throw new Error(`no row for ${right}`);
  return row;
}

export async function automate(steps, call) {
  const report = line => call('e2e.report', { line });
  for (const step of steps) {
    if (step.all) {
      document.querySelector(`button[data-all="${step.all}"]`).click();
    } else if (step.choose) {
      rowOf(step.choose).querySelector(`input[type=radio][value="${step.decision}"]`).click();
    } else if (step.confirm) {
      rowOf(step.confirm).querySelector('input[type=checkbox]').click();
    } else if (step.report) {
      const rows = [...document.querySelectorAll('.right')];
      const risky = rows.filter(row => row.querySelector('.risk')).map(row => row.dataset.right);
      const start = document.getElementById('start');
      const picked = rows.map(row => {
        const radio = row.querySelector('input[type=radio]:checked');
        return `${row.dataset.right}=${radio ? radio.value : 'none'}`;
      });
      await report(`consent-ui rows=${rows.length} risky=${JSON.stringify(risky)} start=${start.disabled ? 'disabled' : 'enabled'} picked=${JSON.stringify(picked)}`);
    } else if (step.submit) {
      document.getElementById('start').click();
    } else if (step.cancel) {
      document.getElementById('cancel').click();
    } else {
      throw new Error(`unknown step ${JSON.stringify(step)}`);
    }
  }
}
