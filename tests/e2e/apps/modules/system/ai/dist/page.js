// The ai module through the real transport against the deterministic FakeProvider of the e2e build
// (docs/stages/m9-ai.md): the roles, the chat stream to its completed end, a tool round trip that
// carries the result of the page's handler back into the next answer, and a break that cancels the
// chat. In the other consent modes the user answered before the start: a denied role, a substituted
// role and a denied tool registration each name their own error.
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

  await check('ai-chat-streams-to-a-completed-end', async () => {
    const parts = [];
    for await (const part of ai.chat({ role: 'smart', messages: [{ role: 'user', content: 'hello' }] })) {
      parts.push(part);
    }
    const text = endOf(parts);
    if (!text.includes('Thinking')) throw new Error(`text: ${JSON.stringify(text)}`);
    const usage = parts.find(part => part.type === 'usage');
    // The FakeProvider of the e2e build: 10 + 5 per round in, 5 + 3 per result out, estimated, complete.
    if (!usage || usage.inputTokens !== 10 || usage.outputTokens !== 5 || usage.estimated !== true || usage.complete !== true) {
      throw new Error(`usage: ${JSON.stringify(usage)}`);
    }
  });

  await check('ai-tool-round-trip-uses-the-handler-result', async () => {
    const unregister = await ai.tools.handle('summarizeSelection', args => ({ summary: `of ${args.text}` }));
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
