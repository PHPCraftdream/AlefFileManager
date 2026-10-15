// SPDX-License-Identifier: MIT OR Apache-2.0
// The `ai` segment: Rush roles, the chat stream and tool handlers (docs/stages/m9-ai.md §2).
// Every function is asynchronous; nothing happens at import time.
import { AlefError } from '../core/errors.ts';
import { on, type Cancelable } from '../core/events.ts';
import { openReadable } from '../core/stream.ts';
import { call } from '../core/transport.ts';

/** What a Rush role can do, as the provider describes it. */
export type RoleCapability = 'text' | 'tool_calls' | 'usage' | 'cancel';

/** A Rush role the application may ask for (`ai.roles`). */
export interface Role {
  id: string;
  title: string;
  description: string;
  capabilities: RoleCapability[];
}

/** One message of the history the application sends; only `user` and `assistant` are allowed. */
export type ChatMessage = { role: 'user'; content: string } | { role: 'assistant'; content: string };

/** One part of a chat as it streams to the page. */
export type ChatPart =
  | { type: 'text'; text: string }
  | { type: 'tool_call'; id: string; name: string; arguments: unknown }
  | {
      type: 'usage';
      inputTokens?: number | null;
      outputTokens?: number | null;
      cacheReadTokens?: number | null;
      cacheCreationTokens?: number | null;
      estimated: boolean;
      complete: boolean;
    }
  | { type: 'end'; reason: 'completed' };

/** What the application asks of `ai.chat`. */
export interface ChatOptions extends Cancelable {
  role: string;
  messages: ChatMessage[];
  tools?: string[];
}

/** What a tool handler learns about the call that invoked it. */
export interface ToolContext {
  callId: string;
  chat: number;
  signal: AbortSignal;
}

type ToolHandler = (args: unknown, context: ToolContext) => unknown;

interface ToolCallEvent {
  chat: number;
  callId: string;
  name: string;
  arguments: unknown;
}

const NAME = /^[a-zA-Z][a-zA-Z0-9_]{0,63}$/;

// Live handlers by tool name; the `ai.tool_call` subscription opens with the first handle().
const handlers = new Map<string, { registration: number; run: ToolHandler }>();
let dispatchOffs: (() => void)[] | undefined;

// Pending handler entries by chat id; a chat that ends (or fails) aborts its handlers.
const pending = new Map<number, Set<{ controller: AbortController; registration: number }>>();

/** Aborts every still-running tool handler of `chat` and forgets the chat. */
function abortChat(chatId: number): void {
  const controllers = pending.get(chatId);
  if (!controllers) return;
  pending.delete(chatId);
  for (const item of controllers) item.controller.abort();
}

/** Registers a chat so its tool handlers can be aborted; the callback unregisters it. */
function trackChat(chatId: number): () => void {
  if (!pending.has(chatId)) pending.set(chatId, new Set());
  return () => abortChat(chatId);
}

/** Runs a handler and reports the outcome; neither the call nor a failure ever surfaces here. */
function report(entry: { registration: number; run: ToolHandler }, event: ToolCallEvent): void {
  const controllers = pending.get(event.chat);
  if (!controllers) return; // the chat already ended (or was never tracked): dispatch nothing
  const controller = new AbortController();
  controllers.add({ controller, registration: entry.registration });
  const context: ToolContext = { callId: event.callId, chat: event.chat, signal: controller.signal };
  Promise.resolve()
    .then(() => entry.run(event.arguments, context))
    .then(
      result => call('ai.tools.result', { chat: event.chat, callId: event.callId, name: event.name, result }),
      () => call('ai.tools.result', { chat: event.chat, callId: event.callId, name: event.name, error: 'the tool handler failed' }),
    )
    .catch(error => console.error('Alef: reporting the tool result failed:', error))
    .finally(() => {
      for (const item of [...controllers]) {
        if (item.controller === controller) controllers.delete(item);
      }
    });
}

/** Dispatches one `ai.tool_call` event to its live handler; foreign or malformed events are ignored. */
function dispatch(payload: unknown): void {
  if (typeof payload !== 'object' || payload === null) return;
  const { chat: chatId, callId, name, arguments: args } = payload as Partial<ToolCallEvent>;
  if (typeof chatId !== 'number' || typeof callId !== 'string' || typeof name !== 'string') return;
  const entry = handlers.get(name);
  if (!entry) return;
  report(entry, { chat: chatId, callId, name, arguments: args });
}

/**
 * The roles Rush offers (`ai.roles`); the application picks one for `chat()`.
 */
export async function roles(): Promise<Role[]> {
  const { roles: list } = await call<{ roles: Role[] }>('ai.roles', null);
  return list;
}

function isMessage(message: unknown): message is ChatMessage {
  if (typeof message !== 'object' || message === null) return false;
  const { role, content } = message as { role?: unknown; content?: unknown };
  return (role === 'user' || role === 'assistant') && typeof content === 'string';
}

/**
 * Streams a chat with the role named `options.role`. Yields `text`, `tool_call`, `usage` and a
 * final `end` part; the iteration opens the stream of the runtime. Breaking out of the loop (or an
 * error) closes the stream, which cancels the chat in the runtime. The arguments are validated
 * here, before anything reaches the runtime.
 */
export function chat(options: ChatOptions): AsyncIterable<ChatPart> {
  const { role, messages, signal } = options ?? {} as ChatOptions;
  if (typeof role !== 'string' || role === '') {
    throw new AlefError('INVALID_ARGUMENT', 'chat(): the role is missing.');
  }
  if (!Array.isArray(messages) || messages.length === 0 || !messages.every(isMessage)) {
    throw new AlefError('INVALID_ARGUMENT', 'chat(): messages must be a non-empty array of user/assistant messages.');
  }
  const args = { role, messages, tools: options.tools ?? [] };
  return iterate();

  async function* iterate(): AsyncGenerator<ChatPart> {
    const { chat: id, stream } = await call<{ chat: number; stream: number }>('ai.chat', args, { signal });
    const untrack = trackChat(id);
    try {
      for await (const frame of await openReadable(stream, { signal })) {
        if (frame.kind !== 'json') continue;
        yield frame.value as ChatPart; // including the public final `end` part, as the runtime sent it
      }
    } finally {
      untrack(); // a break, an error or the end of the stream aborts the handlers of this chat
    }
  }
}

const ai = {
  roles,
  chat,
  tools: {
    /**
     * Handles the tool `name` for every chat of this document. The handler may return a plain
     * value or a promise of one; a synchronous throw is reported as a cleared error, not raised.
     * Resolves with the function that
     * unregisters the tool (idempotent: a second call resolves without talking to the runtime).
     * An `inert` reply means the runtime serves the call but excludes the tool from the model;
     * the round trip and the unregister are identical, so nothing is special-cased here.
     */
    async handle(name: string, handler: ToolHandler): Promise<() => Promise<void>> {
      if (typeof name !== 'string' || !NAME.test(name)) {
        throw new AlefError('INVALID_ARGUMENT', `tools.handle(): "${String(name)}" is not a tool name (letter first, then letters, digits and underscores).`);
      }
      const { registration } = await call<{ registration: number; inert: boolean }>('ai.tools.handle', { name });
      handlers.set(name, { registration, run: handler });
      if (dispatchOffs === undefined) {
        dispatchOffs = [
          await on<ToolCallEvent>('ai.tool_call', dispatch),
          await on<{ chat: number }>('ai.chat_closed', payload => {
            if (typeof payload !== 'object' || payload === null) return;
            const { chat: chatId } = payload as { chat?: unknown };
            if (typeof chatId === 'number') abortChat(chatId);
          }),
        ];
      }
      return async () => {
        const live = handlers.get(name);
        if (!live || live.registration !== registration) return; // already unregistered (or replaced): nothing to do
        handlers.delete(name);
        if (handlers.size === 0) {
          const offs = dispatchOffs;
          dispatchOffs = undefined;
          offs?.forEach(off => off());
        }
        for (const set of pending.values()) {
          for (const item of [...set]) {
            if (item.registration === registration) {
              item.controller.abort();
              set.delete(item);
            }
          }
        }
        await call('ai.tools.unhandle', { registration });
      };
    },
  },
};

export { ai };
