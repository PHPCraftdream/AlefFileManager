// The manager page runs only in the runtime-owned apps process.
const capability = new URLSearchParams(location.hash.slice(1)).get('capability') ?? '';
let token;
export async function call(command, args = null) {
  const response = await fetch(`native://call/${encodeURIComponent(command)}`, {
    method: 'POST', headers: { Authorization: `Bearer ${token ?? capability}`, 'Content-Type': 'application/json' },
    body: command === 'runtime.hello' ? '{}' : JSON.stringify(args),
  });
  if (!response.ok) {
    let payload;
    try { payload = await response.json(); } catch { payload = null; }
    throw new Error(payload?.message ?? payload?.error?.message ?? `${command}: ${response.status}`);
  }
  return response.json();
}
const el = (tag, props = {}, ...children) => {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (key === 'text') node.textContent = value;
    else if (key === 'class') node.className = value;
    else if (key === 'disabled' || key === 'checked' || key === 'hidden' || key === 'open') node[key] = Boolean(value);
    else node.setAttribute(key, value);
  }
  node.append(...children); return node;
};
const rightText = right => right.scope == null ? right.permission : `${right.permission}:${right.scope}`;
const nameOf = app => app.name ?? app.id;
export let ready;
let busy = 0;
let wake = () => {};
export let settled = Promise.resolve();
export let view;
function begin() {
  if (busy === 0) settled = new Promise(resolve => { wake = resolve; });
  busy += 1;
}
function end() {
  busy = Math.max(0, busy - 1);
  if (busy === 0) { const done = wake; wake = () => {}; done(); }
}
function showError(node, error) { node.textContent = error instanceof Error ? error.message : String(error); }
function render(root, initial) {
  let entries = initial; let selected; let visible = [];
  root.replaceChildren(el('h1', { text: 'Applications' }), el('p', { class: 'sub', text: 'Review and change what each application may do.' }));
  const filter = el('input', { type: 'search', 'aria-label': 'Search applications', placeholder: 'Search name, id, version or source' });
  const list = el('div', { class: 'apps', role: 'list', 'aria-label': 'Applications' });
  const content = el('section', { class: 'panel', 'aria-label': 'Application details' });
  root.append(el('div', { class: 'layout' }, el('section', { class: 'panel' }, filter, list), content));
  const sameIdentity = (a, b) => a?.identity.app_id === b?.identity.app_id && a?.identity.signer === b?.identity.signer;
  function listItems() {
    list.replaceChildren();
    visible = entries.filter(app => `${nameOf(app)} ${app.id} ${app.version ?? ''} ${app.source ?? ''}`.toLocaleLowerCase().includes(filter.value.toLocaleLowerCase()));
    if (!visible.length) list.append(el('p', { class: 'empty', text: entries.length ? 'No applications match your search.' : 'No applications found.' }));
    for (const app of visible) {
      const button = el('button', { class: 'app', type: 'button', role: 'listitem', 'aria-current': String(sameIdentity(selected, app)) },
        el('strong', { text: nameOf(app) }), el('small', { text: `${app.id} · ${app.version ?? '—'} · ${app.source ?? '—'}` }),
        el('small', { text: `allow ${app.summary.allow} · substitute ${app.summary.substitute} · deny ${app.summary.deny} · undecided ${app.summary.undecided} · red ${app.summary.red}` }));
      button.addEventListener('click', () => select(app)); list.append(button);
    }
  }
  async function refreshList() {
    entries = await call('runtime.apps.list');
    if (selected) selected = entries.find(app => sameIdentity(app, selected)) ?? selected;
    listItems();
  }
  function select(app) { begin(); selected = app; listItems(); void loadDetails(app); }
  async function loadDetails(app) {
    content.replaceChildren(el('h2', { text: nameOf(app) }), el('p', { class: 'muted', text: `${app.id} · ${app.version ?? '—'} · ${app.source ?? '—'} · ${app.availability}` }));
    const topError = el('p', { class: 'error', role: 'alert' });
    try {
      const description = await call('runtime.apps.describe', { identity: app.identity });
      if (!sameIdentity(selected, app)) return;
      const toolbar = el('div', { class: 'toolbar' });
      const resetButton = el('button', { type: 'button', text: 'Reset decisions' });
      resetButton.addEventListener('click', async () => {
        begin(); resetButton.disabled = true; topError.textContent = '';
        try {
          await call('runtime.apps.reset', { identity: app.identity });
          await refreshList();
          await renderDetails(app);
        } catch (error) {
          showError(topError, error);
          resetButton.disabled = false;
        }
        finally { end(); }
      });
      const close = el('button', { type: 'button', text: 'Close' });
      close.addEventListener('click', () => call('runtime.apps.close').catch(error => showError(topError, error)));
      toolbar.append(resetButton, close); content.append(toolbar, topError);
      if (app.availability === 'damaged') content.append(el('p', { class: 'error', text: 'The installed application record is damaged.' }));
      if (app.availability !== 'available') content.append(el('p', { class: 'empty', text: 'The source is unavailable; saved rights are shown read-only. Reset is available, but changes cannot take effect until the source is available.' }));
      content.append(el('p', { class: 'note', text: 'Taking a right away narrows a running application within about 500 ms. Giving a right or widening it takes effect on the next launch (except webcrypto, whose decision is set when the document starts). Reset leaves rights undecided; the next launch asks again.' }));
      for (const entry of description.rights) {
        const label = rightText(entry.right); const rowError = el('p', { class: 'error', role: 'alert' });
        const choices = el('fieldset', { class: 'choices', 'aria-label': `Decision for ${label}` }); const controls = {};
        for (const decision of ['allow', 'substitute', 'deny']) {
          const radio = el('input', { type: 'radio', name: `right-${label}`, value: decision });
          controls[decision] = radio; radio.checked = entry.decision === decision;
          choices.append(el('label', {}, radio, document.createTextNode(decision[0].toUpperCase() + decision.slice(1))));
        }
        if (app.availability !== 'available') {
          for (const radio of Object.values(controls)) radio.disabled = true;
        }
        const checkbox = el('input', { type: 'checkbox' }); let explicitlyConfirmed = false;
        if (!entry.red) checkbox.disabled = true;
        const confirmation = el('div', { class: 'confirm' }, el('label', {}, checkbox, document.createTextNode('I understand this red right bypasses all protections, including the AI model keys.')));
        confirmation.hidden = !entry.red; checkbox.addEventListener('change', () => { explicitlyConfirmed = checkbox.checked; });
        const row = el('div', { class: entry.red ? 'right red' : 'right' }, el('div', { class: 'what', text: label }), choices);
        if (entry.description) row.append(el('div', { class: 'detail', text: entry.description }));
        if (entry.red) row.append(confirmation);
        const save = el('button', { type: 'button', text: 'Save decision', disabled: app.availability !== 'available' });
        save.addEventListener('click', async () => {
          begin(); const decision = Object.keys(controls).find(key => controls[key].checked);
          try {
            rowError.textContent = '';
            if (!decision) throw new Error('Choose a decision first.');
            save.disabled = true;
            await call('runtime.apps.set', {
              identity: app.identity,
              right: entry.right,
              decision,
              confirm_red: entry.red && explicitlyConfirmed,
            });
            explicitlyConfirmed = false; checkbox.checked = false;
            await refreshList();
            await renderDetails(app);
          } catch (error) {
            showError(rowError, error);
            save.disabled = app.availability !== 'available';
          }
          finally { end(); }
        });
        for (const radio of Object.values(controls)) {
          radio.addEventListener('change', () => {
            explicitlyConfirmed = false;
            checkbox.checked = false;
            rowError.textContent = '';
          });
        }
        row.append(save, rowError); content.append(row);
      }
    } catch (error) { content.append(el('p', { class: 'error', role: 'alert', text: `Application details cannot be shown: ${error.message}` })); }
    finally { end(); }
  }
  async function renderDetails(app) {
    selected = entries.find(row => sameIdentity(row, app)) ?? app;
    begin();
    listItems();
    await loadDetails(selected);
  }
  filter.addEventListener('input', listItems); listItems();
  if (entries.length) select(entries[0]); else content.append(el('p', { class: 'empty', text: 'No applications found.' }));
  return { entries: () => entries, visible: () => visible, selected: () => selected };
}
async function main() {
  try { const hello = await call('runtime.hello'); token = hello.token; view = render(document.getElementById('root'), await call('runtime.apps.list')); return view; }
  catch (error) { document.getElementById('root').textContent = `Applications cannot be shown: ${error.message}`; }
}
ready = main();
