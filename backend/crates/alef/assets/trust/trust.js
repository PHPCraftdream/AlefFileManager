// The trust page of the runtime: shows who signs an application and asks whether it is trusted.
// It runs only in the process `alef trust`; the document of an application never gets the commands
// used here (`runtime.trust.*`). Everything that comes from the request is text of a stranger: it
// is only ever put in with `textContent`.

// The transport of the runtime, as much of it as this page needs (see packages/api/src/core).
const capability = new URLSearchParams(location.hash.slice(1)).get('capability') ?? '';
let token;

export async function call(command, args = null) {
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

function render(request) {
  const root = document.getElementById('root');
  root.replaceChildren();
  const trust = request.trust ?? {};

  const accept = element('button', { class: 'primary', type: 'button', id: 'accept', text: 'Accept' });
  const refuse = element('button', { type: 'button', id: 'refuse', text: 'Refuse' });
  const hint = element('span', { class: 'hint' });

  root.append(
    element('h1', { text: `Do you trust ${request.app.name}?` }),
    element('p', { class: 'sub', text: `${request.app.id} ${request.app.version}` }),
  );

  if (trust.fingerprint !== undefined) {
    root.append(
      element('section', { class: 'signed' },
        element('h2', { text: 'Signed' }),
        element('p', {
          text: 'The application is signed. Verify this fingerprint with its publisher before you trust it:',
        }),
        element('div', { class: 'fingerprint', text: trust.fingerprint }),
      ),
    );
  } else {
    root.append(
      element('section', { class: 'unsigned' },
        element('h2', { text: '⚠ Not signed' }),
        element('p', {
          text: 'This application is NOT signed. It is identified only by the digest of these exact bytes:',
        }),
        element('div', { class: 'fingerprint', text: trust.archive_digest ?? '' }),
      ),
    );
  }

  accept.addEventListener('click', async () => {
    accept.disabled = true;
    refuse.disabled = true;
    hint.textContent = 'Accepting\u2026';
    await call('runtime.trust.answer', { decisions: [], trust: true });
  });
  refuse.addEventListener('click', async () => {
    accept.disabled = true;
    refuse.disabled = true;
    hint.textContent = 'Refusing\u2026';
    await call('runtime.trust.answer', { decisions: [], trust: false });
  });

  document.body.append(element('footer', {}, hint, refuse, accept));
}

async function main() {
  const hello = await call('runtime.hello');
  token = hello.token;
  const request = await call('runtime.trust.request');
  render(request);
  return request;
}

export const ready = main();
ready.catch(error => {
  document.getElementById('root').textContent = `The trust question cannot be shown: ${error.message}`;
});
