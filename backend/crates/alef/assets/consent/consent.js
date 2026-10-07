// The consent page of the runtime: shows what an application asks for and sends back what the user
// decided. It runs only in the process `alef consent`; the document of an application never gets the
// commands used here (`runtime.consent.*`). Everything that comes from the manifest of the
// application is text of a stranger: it is only ever put in with `textContent`.

const DECISIONS = ['allow', 'substitute', 'deny'];
const LABELS = { allow: 'Allow', substitute: 'Substitute', deny: 'Deny' };

const GROUPS = [
  ['fs', 'Files'],
  ['net', 'Network'],
  ['cli', 'Programs'],
  ['shell', 'Opening addresses'],
  ['app', 'Environment'],
  ['clipboard', 'Clipboard'],
  ['window', 'Windows'],
];

// The transport of the runtime, as much of it as this page needs (see packages/api/src/core).
const capability = new URLSearchParams(location.hash.slice(1)).get('capability') ?? '';
let token;

async function call(command, args = null) {
  const bearer = token ?? capability;
  const response = await fetch(`native://call/${encodeURIComponent(command)}`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${bearer}`, 'Content-Type': 'application/json' },
    body: command === 'runtime.hello' ? '{}' : JSON.stringify(args),
  });
  if (!response.ok) throw new Error(`${command}: ${response.status}`);
  return response.json();
}

function element(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (key === 'text') node.textContent = value;
    else if (key === 'class') node.className = value;
    else node.setAttribute(key, value);
  }
  node.append(...children);
  return node;
}

const keyOf = right => `${right.permission}\u0000${right.scope ?? ''}`;
const textOf = right => (right.scope === undefined ? right.permission : `${right.permission}:${right.scope}`);

function groupOf(permission) {
  const head = permission.split('.')[0];
  return GROUPS.find(([name]) => name === head) ?? ['other', 'Other'];
}

function render(request) {
  const root = document.getElementById('root');
  root.replaceChildren();
  const state = new Map(); // key -> { right, decision, confirmed, row, confirmBox, radios }

  root.append(
    element('h1', { text: `${request.app.name} asks for permissions` }),
    element('p', { class: 'sub', text: `${request.app.id} ${request.app.version}` }),
    element('p', {
      class: 'note',
      text: 'You decide for each right: allow it, substitute it (the application gets a believable stand-in and no error that gives it away) or deny it. Nothing here is decided for you. A stand-in is not a hiding place from an application that looks for one.',
    }),
  );

  const start = element('button', { class: 'primary', type: 'button', id: 'start', text: 'Start' });
  const cancel = element('button', { type: 'button', id: 'cancel', text: 'Cancel' });
  const hint = element('span', { class: 'hint' });

  function refresh() {
    let open = 0;
    for (const item of state.values()) {
      const needsConfirm = item.right.risk !== undefined && item.decision === 'allow' && !item.confirmed;
      if (item.decision === undefined || needsConfirm) open += 1;
      item.confirmBox.disabled = !(item.right.risk !== undefined && item.decision === 'allow');
    }
    start.disabled = open > 0;
    hint.textContent = open > 0 ? `${open} right${open === 1 ? '' : 's'} left to decide` : 'Everything is decided';
  }

  function choose(item, decision) {
    item.decision = decision;
    item.radios[decision].checked = true;
    if (decision !== 'allow') {
      item.confirmed = false;
      item.confirmBox.checked = false;
    }
    refresh();
  }

  const bulk = element('div', { class: 'bulk' });
  for (const decision of DECISIONS) {
    const button = element('button', { type: 'button', 'data-all': decision, text: `${LABELS[decision]} all` });
    button.addEventListener('click', () => {
      for (const item of state.values()) choose(item, decision);
    });
    bulk.append(button);
  }
  root.append(bulk);

  const sections = new Map();
  let index = 0;
  for (const right of request.rights) {
    const [name, title] = groupOf(right.permission);
    if (!sections.has(name)) {
      const section = element('section', {}, element('h2', { text: title }));
      sections.set(name, section);
      root.append(section);
    }
    index += 1;
    const radios = {};
    const choices = element('fieldset', { 'aria-label': `Decision for ${textOf(right)}` });
    for (const decision of DECISIONS) {
      const radio = element('input', { type: 'radio', name: `right-${index}`, value: decision });
      radios[decision] = radio;
      choices.append(element('label', {}, radio, document.createTextNode(LABELS[decision])));
    }
    const confirmBox = element('input', { type: 'checkbox', disabled: '' });
    const row = element('div', { class: 'right', 'data-right': textOf(right) },
      element('div', { class: 'what', text: textOf(right) }),
      choices);
    if (right.risk !== undefined) {
      row.append(element('div', { class: 'risk' },
        element('span', { text: `⚠ ${right.risk}` }),
        element('label', {}, confirmBox, document.createTextNode(' I understand, allow it'))));
    }
    const item = { right, decision: undefined, confirmed: false, confirmBox, radios };
    state.set(keyOf(right), item);
    for (const decision of DECISIONS) radios[decision].addEventListener('change', () => choose(item, decision));
    confirmBox.addEventListener('change', () => {
      item.confirmed = confirmBox.checked;
      refresh();
    });
    sections.get(name).append(row);
    if (right.decision !== undefined) choose(item, right.decision);
    if (right.decision === 'allow' && right.risk !== undefined) {
      item.confirmed = true;
      confirmBox.checked = true;
    }
  }

  start.addEventListener('click', async () => {
    start.disabled = true;
    const decisions = [...state.values()].map(({ right, decision, confirmed }) => ({
      permission: right.permission,
      ...(right.scope === undefined ? {} : { scope: right.scope }),
      decision,
      confirmed,
    }));
    await call('runtime.consent.answer', { decisions });
  });
  cancel.addEventListener('click', () => call('runtime.consent.cancel'));

  document.body.append(element('footer', {}, hint, cancel, start));
  refresh();
  return { state };
}

async function main() {
  const hello = await call('runtime.hello');
  token = hello.token;
  const request = await call('runtime.consent.request');
  render(request);
  if (request.automation) {
    const { automate } = await import('./consent-e2e.js');
    await automate(request.automation, call);
  }
}

main().catch(error => {
  document.getElementById('root').textContent = `The permissions cannot be shown: ${error.message}`;
});
