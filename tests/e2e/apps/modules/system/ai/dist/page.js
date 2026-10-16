// The M9.2 AI surface: roles, requests, session history, application tools and scoped builtins.
import { api, guard, rejection, suite, until, verdict } from './harness.js';

const { ai } = api;

/** Collects the text of the parts and fails unless the stream ended completed exactly once. */
function endOf(parts) {
  const ends = parts.filter(part => part.type === 'end');
  if (ends.length !== 1 || ends[0].reason !== 'completed') throw new Error(`end: ${JSON.stringify(ends)}`);
  if (parts.at(-1) !== ends[0]) throw new Error('the end part is not the last part');
  return parts.filter(part => part.type === 'text').map(part => part.text).join('');
}

async function allowed(t, check) {
  await check('ai-roles-list-the-allowed-role', async () => {
    const roles = await ai.roles();
    const smart = roles.find(role => role.id === 'smart');
    if (!smart) throw new Error(`no smart among ${JSON.stringify(roles.map(role => role.id))}`);
    if (typeof smart.title !== 'string' || typeof smart.description !== 'string') throw new Error('the shape of the role');
    if (!Array.isArray(smart.capabilities) || smart.capabilities.length === 0) throw new Error('no capabilities');
    if (roles.length !== 1) throw new Error(`${roles.length} roles, expected only the allowed one`);
  });

  await check('ai-request-api-streams-to-a-completed-end', async () => {
    const parts = [];
    for await (const part of ai.request({ role: 'smart', messages: [{ role: 'user', content: 'hello' }] })) parts.push(part);
    if (!endOf(parts).includes('Thinking')) throw new Error('request had no text response');
  });

  await check('ai-session-history-is-carried-between-sends', async () => {
    const session = await ai.session({ role: 'smart' });
    try {
      const first = [];
      for await (const part of session.send({ messages: [{ role: 'user', content: 'hello' }] })) first.push(part);
      const firstText = endOf(first);
      const second = [];
      for await (const part of session.send({ messages: [{ role: 'user', content: 'follow up' }] })) second.push(part);
      const secondText = endOf(second);
      if (!secondText.startsWith('History ')) throw new Error(`unexpected follow-up: ${secondText}`);
      const history = JSON.parse(secondText.slice('History '.length));
      if (!Array.isArray(history) || history.length !== 3
        || history[0]?.role !== 'user' || history[0]?.content !== 'hello'
        || history[1]?.role !== 'assistant' || history[1]?.content !== firstText
        || history[2]?.role !== 'user' || history[2]?.content !== 'follow up') {
        throw new Error(`unexpected history: ${JSON.stringify(history)}`);
      }
    } finally { await session.close(); }
  });

  await check('ai-session-without-tools-streams', async () => {
    const session = await ai.session({ role: 'smart' });
    try {
      const parts = [];
      for await (const part of session.send({ messages: [{ role: 'user', content: 'hello' }] })) parts.push(part);
      const text = endOf(parts);
      if (!text.startsWith('History ')) throw new Error(`session response: ${text}`);
      if (parts.some(part => part.type === 'tool_call')) throw new Error('session without tools emitted a tool_call');
    } finally { await session.close(); }
  });

  await check('ai-builtin-inside-scope-returns-sentinel-content', async () => {
    const parts = [];
    for await (const part of ai.request({ role: 'smart', messages: [{ role: 'user', content: JSON.stringify({ path: t.aiInside }) }], builtin: ['fs.read'] })) parts.push(part);
    const text = endOf(parts);
    if (!text.includes(t.aiSentinel)) throw new Error(`inside-scope content missing: ${text}`);
  });
  await check('ai-builtin-outside-scope-is-denied', async () => {
    const error = await rejection((async () => { for await (const _part of ai.request({ role: 'smart', messages: [{ role: 'user', content: JSON.stringify({ path: t.aiOutside }) }], builtin: ['fs.read'] })) {} })());
    if (error?.code !== 'PERMISSION_DENIED') throw new Error(`${error?.code ?? error?.name}, expected PERMISSION_DENIED`);
  });

  await check('ai-red-absent-and-exec-denied', async () => {
    const roles = await ai.roles();
    if (roles.some(role => role.id === 'red')) throw new Error('red role must not be exposed');
    const error = await rejection((async () => {
      for await (const _part of ai.request({ role: 'smart', messages: [{ role: 'user', content: 'hello' }], builtin: ['exec'] })) {}
    })());
    if (error?.code !== 'PERMISSION_DENIED') throw new Error(`${error?.code ?? error?.name}, expected PERMISSION_DENIED for absent exec right`);
  });


  await check('ai-chat-streams-to-a-completed-end', async () => {
    const parts = [];
    for await (const part of ai.chat({ role: 'smart', messages: [{ role: 'user', content: 'hello' }] })) {
      parts.push(part);
    }
    const text = endOf(parts);
    if (!text.includes('Thinking')) throw new Error(`text: ${JSON.stringify(text)}`);
    const usage = parts.find(part => part.type === 'usage');
    if (!usage || usage.inputTokens !== 10 || usage.outputTokens !== 5 || usage.estimated !== true || usage.complete !== true) {
      throw new Error(`usage: ${JSON.stringify(usage)}`);
    }
  });

  await check('ai-tool-round-trip-uses-the-handler-result', async () => {
    const unregister = await ai.tools.handle('summarizeSelection', args => ({ summary: `of ${args.input}` }));
    try {
      const parts = [];
      for await (const part of ai.chat({
        role: 'smart', messages: [{ role: 'user', content: 'summarize' }], tools: ['summarizeSelection'],
      })) {
        parts.push(part);
      }
      const calls = parts.filter(part => part.type === 'tool_call');
      if (calls.length !== 1 || calls[0].name !== 'summarizeSelection') throw new Error(`tool calls: ${JSON.stringify(calls)}`);
      if (JSON.stringify(calls[0].arguments) !== JSON.stringify({ input: 'ping' })) throw new Error(`arguments: ${JSON.stringify(calls[0].arguments)}`);
      const text = endOf(parts);
      if (!text.includes('Saw 1 tool result(s)') || !text.includes('of ping')) throw new Error(`the handler result did not come back: ${JSON.stringify(text)}`);
      const usage = parts.find(part => part.type === 'usage');
      if (!usage || usage.inputTokens !== 15 || usage.outputTokens !== 8) throw new Error(`usage: ${JSON.stringify(usage)}`);
    } finally {
      await unregister();
    }
  });

  await check('ai-cancel-by-break-closes-the-chat', async () => {
    // The handler never answers: the chat only moves because the page breaks out of the iteration,
    // which closes the stream and must abort the still-pending handler of this chat.
    let aborted = false;
    const unregister = await ai.tools.handle('summarizeSelection', (_args, context) => new Promise((_resolve, reject) => {
      context.signal.addEventListener('abort', () => { aborted = true; reject(new Error('the chat was closed')); });
    }));
    try {
      const parts = [];
      for await (const part of ai.chat({
        role: 'smart', messages: [{ role: 'user', content: 'cancel me' }], tools: ['summarizeSelection'],
      })) {
        parts.push(part);
        if (part.type === 'tool_call') break;
      }
      await until(() => aborted, 5000, 'the signal of the pending handler to abort');
      const text = parts.filter(part => part.type === 'text').map(part => part.text).join('');
      if (text.includes('Saw')) throw new Error(`the chat went on after the break: ${JSON.stringify(text)}`);
      if (parts.some(part => part.type === 'end')) throw new Error('an end part arrived after the break');
    } finally {
      await unregister();
    }
  });
}

/** The user denied `ai.role:smart`: the call is refused like a right the manifest never listed. */
async function roleDenied(t, check) {
  await check('ai-denied-role-is-permission-denied', async () => {
    const listed = await ai.roles();
    if (listed.some(role => role.id === 'smart')) throw new Error('the denied role is still listed');
    const chat = async () => {
      for await (const part of ai.chat({ role: 'smart', messages: [{ role: 'user', content: 'hello' }] })) {
        if (part.type === 'end') return;
      }
    };
    const error = await rejection(chat());
    if (error?.code !== 'PERMISSION_DENIED') throw new Error(`${error?.code ?? error?.name ?? 'it succeeded'}, expected PERMISSION_DENIED`);
  });
}

/** The user substituted `ai.role:smart`: the chat answers NOT_AVAILABLE and nothing is sent. */
async function roleSubstituted(t, check) {
  await check('ai-substituted-role-is-not-available', async () => {
    const chat = async () => {
      for await (const part of ai.chat({ role: 'smart', messages: [{ role: 'user', content: 'hello' }] })) {
        if (part.type === 'end') return;
      }
    };
    const error = await rejection(chat());
    if (error?.code !== 'NOT_AVAILABLE') throw new Error(`${error?.code ?? error?.name ?? 'it succeeded'}, expected NOT_AVAILABLE`);
  });
}

/** The user denied `ai.tool:summarizeSelection`: the registration is refused before any chat. */
async function toolDenied(t, check) {
  await check('ai-denied-tool-registration-is-permission-denied', async () => {
    const error = await rejection(ai.tools.handle('summarizeSelection', async () => null));
    if (error?.code !== 'PERMISSION_DENIED') throw new Error(`${error?.code ?? error?.name ?? 'it succeeded'}, expected PERMISSION_DENIED`);
  });
}

const MODES = {
  allowed,
  'role-denied': roleDenied,
  'role-substituted': roleSubstituted,
  'tool-denied': toolDenied,
};

async function main() {
  const t = await (await fetch('targets.json')).json();
  const run = MODES[t.mode] ?? allowed;
  const { check, failed } = suite();
  await run(t, check);
  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
