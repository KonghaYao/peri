import type { Database } from 'bun:sqlite';
import type {
  ChatExecuteResult,
  ChatFactsResult,
  ChatOpenResult,
  ChatOpenTarget,
} from '@peri-code/rivet/rivet';
import type { ChatBindingTarget, ChatFactEnvelope, ChatUserAction, InteractionAnswer } from '@peri-code/sdk';
import { type AgentDraft, AgentDraft as AgentDraftSchema } from '@shared/agent';
import type { ChatRuntimeStatus } from '@shared/chat';
import { Hono } from 'hono';
import { AgentsRepository } from '../../../../server/agents/repository';
import type { ChatActorPort, ChatEngine } from '../../../../server/chats/engine';
import { ChatsRepository } from '../../../../server/chats/repository';
import { ChatService } from '../../../../server/chats/service';
import { openDatabase } from '../../../../server/db/database';
import { migrate } from '../../../../server/db/migrate';
import {
  createPlatformRepositories,
  type PlatformRepositories,
} from '../../../../server/platform/repositories';
import { createPlatformRoutes } from '../../../../server/platform/routes';
import { SessionRecorder } from '../../../../server/platform/sessions/recorder';
import {
  createSessionRuntime,
  installSessionRuntime,
  sessionDraftFor,
} from '../../../../server/platform/sessions/runtime';
import type { SandboxCoordinator } from '../../../../server/sandbox/coordinator';
import { testEnv, withStoredProvider } from '../../helpers';

export const AUTHORIZATION = { authorization: 'Bearer test-token' } as const;

export type Fact = ChatFactEnvelope['fact'];

/** Sandbox seam with observable holder acquisition; returns no injection so targets stay local. */
export class RecordingSandbox {
  readonly enabled = true;
  readonly acquired: string[] = [];
  readonly released: string[] = [];

  async acquire(chatId: string) {
    this.acquired.push(chatId);
    return null;
  }

  async release(chatId: string) {
    this.released.push(chatId);
    return { released: true, errors: [] as string[] };
  }

  state() {
    return { state: 'disabled' as const, reason: null };
  }
}

/**
 * Chat actor driven entirely by explicit facts. Nothing is inferred from `execute`: tests decide
 * when a turn starts, streams or completes, exactly like the SDK's fact stream.
 */
export class SessionFakeActor implements ChatActorPort {
  readonly chatId: string;
  readonly actions: ChatUserAction[] = [];
  readonly answers: InteractionAnswer[] = [];
  /** Every target handed to `open`, in order; sessions freeze one target per activation. */
  readonly openTargets: ChatOpenTarget[] = [];
  binding: ChatBindingTarget;
  envelopes: ChatFactEnvelope[] = [];
  #sequence = 0;
  #listeners = new Set<(envelope: ChatFactEnvelope) => void>();
  closed = false;

  constructor(chatId: string) {
    this.chatId = chatId;
    this.binding = { chatId, sessionId: `${chatId}-runtime`, bindingRevision: 1 };
  }

  async open(target: ChatOpenTarget): Promise<ChatOpenResult> {
    this.openTargets.push(structuredClone(target));
    return {
      chatId: target.chatId,
      workspace: target.workspace,
      binding: { ...this.binding, chatId: target.chatId },
      reused: false,
    };
  }

  async execute(action: ChatUserAction): Promise<ChatExecuteResult> {
    this.actions.push(action);
    switch (action.type) {
      case 'send':
        return {
          type: 'send',
          requestId: action.requestId,
          inputId: `input-${this.actions.length}`,
          target: { ...this.binding },
        };
      case 'cancel':
        return { type: 'cancel', result: { requestId: action.requestId, status: 'requested' } };
      case 'answer':
        this.answers.push(action.payload.answer);
        return { type: 'answer', result: { requestId: action.requestId, status: 'accepted' } };
      case 'create_new_session':
        return { type: 'create_new_session', result: { ...this.binding, requestId: action.requestId } };
      case 'change_session':
        return {
          type: 'change_session',
          result: {
            ...this.binding,
            sessionId: action.payload.sessionId,
            requestId: action.requestId,
          },
        };
      default:
        throw Object.assign(new Error(`unsupported action ${action.type}`), { code: 'invalid_action' });
    }
  }

  async facts(): Promise<ChatFactsResult> {
    return { binding: { ...this.binding }, envelopes: [...this.envelopes], overflow: false };
  }

  async close(): Promise<{ closed: true }> {
    this.closed = true;
    return { closed: true };
  }

  async onFact(listener: (envelope: ChatFactEnvelope) => void): Promise<() => Promise<void>> {
    this.#listeners.add(listener);
    return async () => {
      this.#listeners.delete(listener);
    };
  }

  emit(fact: Fact): void {
    this.#sequence += 1;
    const envelope: ChatFactEnvelope = {
      chatId: this.chatId,
      sessionId: this.binding.sessionId,
      bindingRevision: this.binding.bindingRevision,
      sequence: this.#sequence,
      fact,
    };
    this.envelopes.push(envelope);
    for (const listener of [...this.#listeners]) listener(envelope);
  }

  /** Starts the next binding generation; the hub must rebuild from the new fact set. */
  rebind(envelopes: ChatFactEnvelope[] = []): number {
    this.binding = {
      ...this.binding,
      bindingRevision: this.binding.bindingRevision + 1,
    };
    this.envelopes = envelopes.map((envelope) => ({
      ...envelope,
      bindingRevision: this.binding.bindingRevision,
    }));
    this.#sequence = this.envelopes.length;
    return this.binding.bindingRevision;
  }

  runState(running: boolean): void {
    this.emit({ kind: 'run-state', running, executionBlocked: false });
  }

  input(inputId: string, text: string): void {
    this.emit({ kind: 'input-delivered', inputId, text });
  }

  chunk(text: string, messageId = 'm-1'): void {
    this.#notice({
      sessionUpdate: 'agent_message_chunk',
      messageId,
      content: { type: 'text', text },
    });
  }

  thought(text: string): void {
    this.#notice({
      sessionUpdate: 'agent_thought_chunk',
      messageId: 'r-1',
      content: { type: 'text', text },
    });
  }

  tool(update: {
    toolCallId: string;
    title: string;
    kind?: string;
    status?: string;
    arguments?: unknown;
    result?: unknown;
  }): void {
    this.#notice({ sessionUpdate: 'tool_call', ...update });
  }

  usage(inputTokens: number, outputTokens: number): void {
    this.#notice({
      sessionUpdate: 'usage_update',
      used: inputTokens + outputTokens,
      size: 200_000,
      _meta: { inputTokens, outputTokens },
    });
  }

  turnComplete(status: 'completed' | 'cancelled' | 'error' = 'completed'): void {
    this.emit({ kind: 'turn-complete', status });
  }

  interaction(id: string, kind: 'permission' | 'elicitation', turnId: string): void {
    this.emit({
      kind: 'interaction-began',
      record: { id, kind, sessionId: this.binding.sessionId, turnId, toolCallId: 'call-1', title: 'Shell' },
    });
  }

  #notice(update: Record<string, unknown>): void {
    this.emit({
      kind: 'notice',
      notification: {
        jsonrpc: '2.0',
        method: 'session/update',
        params: { sessionId: this.binding.sessionId, update },
      } as never,
    });
  }
}

export class SessionFakeEngine implements ChatEngine {
  readonly status: ChatRuntimeStatus = { mode: 'wasm', reasons: [] };
  readonly endpoint = 'http://127.0.0.1:8961';
  readonly actors = new Map<string, SessionFakeActor>();
  started = 0;

  start(): Promise<void> {
    this.started += 1;
    return Promise.resolve();
  }

  health() {
    return { mode: 'embedded', status: 'ready' } as const;
  }

  actor(chatId: string): SessionFakeActor {
    let actor = this.actors.get(chatId);
    if (actor === undefined) {
      actor = new SessionFakeActor(chatId);
      this.actors.set(chatId, actor);
    }
    return actor;
  }

  sandbox(): null {
    return null;
  }

  shutdown(): Promise<void> {
    return Promise.resolve();
  }
}

const DRAFT: AgentDraft = AgentDraftSchema.parse({
  name: 'Session Agent',
  description: '',
  config: {
    instructions: 'Be brief',
    provider: { id: 'primary', type: 'anthropic', model: 'claude-sonnet-4-5' },
    metaHarness: {},
    mcpServers: {},
  },
});

export interface Harness {
  readonly db: Database;
  readonly repos: PlatformRepositories;
  readonly engine: SessionFakeEngine;
  readonly chats: ChatService;
  readonly recorder: SessionRecorder;
  readonly app: Hono;
  readonly agentId: string;
  readonly sandbox: RecordingSandbox | null;
  actor(sessionId: string): SessionFakeActor;
  dispose(): Promise<void>;
}

export function setup(options: { sandbox?: boolean } = {}): Harness {
  const db = openDatabase(':memory:');
  migrate(db);
  const agents = new AgentsRepository(db);
  const agent = agents.create(withStoredProvider(db, structuredClone(DRAFT)));
  const engine = new SessionFakeEngine();
  const sandbox = options.sandbox === true ? new RecordingSandbox() : null;
  const repos = createPlatformRepositories(db);
  const recorder = new SessionRecorder({ repos });
  const chats = new ChatService({
    env: testEnv(),
    engine,
    chats: new ChatsRepository(db),
    agents,
    draftFor: sessionDraftFor(repos),
    ...(sandbox === null ? {} : { sandbox: sandbox as unknown as SandboxCoordinator }),
  });
  const restore = installSessionRuntime(createSessionRuntime({ repos, chats, recorder }));
  const app = new Hono();
  app.route('/v1', createPlatformRoutes({ db }));

  return {
    db,
    repos,
    engine,
    chats,
    recorder,
    app,
    agentId: agent.id,
    sandbox,
    actor: (sessionId) => engine.actor(sessionId),
    async dispose() {
      restore();
      db.close();
    },
  };
}

export const tick = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

export async function json<T = Record<string, unknown>>(response: Response): Promise<T> {
  return (await response.json()) as T;
}

export interface EventRow {
  readonly seq: number;
  readonly event_id: string;
  readonly type: string;
  readonly turn_id: string | null;
  readonly item_id: string | null;
  readonly data: Record<string, unknown>;
}

export function sessionEvents(harness: Harness, sessionId: string): EventRow[] {
  return harness.repos.events.listBySession(sessionId).map((row) => ({
    seq: row.seq,
    event_id: row.event_id,
    type: row.type,
    turn_id: row.turn_id,
    item_id: row.item_id,
    data: JSON.parse(row.data) as Record<string, unknown>,
  }));
}

export function eventTypes(harness: Harness, sessionId: string): string[] {
  return sessionEvents(harness, sessionId).map((event) => event.type);
}

/** Reads SSE frames until `count` events arrive; cancels the stream afterwards. */
export async function readFrames(
  response: Response,
  count: number,
  timeoutMs = 2000,
): Promise<{ id: string | null; event: string; data: Record<string, unknown> }[]> {
  const body = response.body;
  if (body === null) throw new Error('response has no body');
  const reader = body.getReader();
  const decoder = new TextDecoder();
  const frames: { id: string | null; event: string; data: Record<string, unknown> }[] = [];
  let buffer = '';
  const deadline = Date.now() + timeoutMs;
  try {
    while (frames.length < count) {
      if (Date.now() > deadline) throw new Error(`timed out after ${frames.length} frames`);
      const read = await withTimeout(reader.read(), timeoutMs);
      if (read.done) break;
      buffer += decoder.decode(read.value, { stream: true });
      let split = buffer.indexOf('\n\n');
      while (split !== -1 && frames.length < count) {
        const block = buffer.slice(0, split);
        buffer = buffer.slice(split + 2);
        const frame = parseFrame(block);
        if (frame !== null) frames.push(frame);
        split = buffer.indexOf('\n\n');
      }
    }
  } finally {
    await reader.cancel().catch(() => {});
  }
  return frames;
}

function withTimeout<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('sse read timeout')), timeoutMs);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

function parseFrame(
  block: string,
): { id: string | null; event: string; data: Record<string, unknown> } | null {
  let id: string | null = null;
  let event = '';
  let data = '';
  for (const line of block.split('\n')) {
    if (line.startsWith('id: ')) id = line.slice(4);
    else if (line.startsWith('event: ')) event = line.slice(7);
    else if (line.startsWith('data: ')) data += line.slice(6);
  }
  if (event.length === 0 || data.length === 0) return null;
  return { id, event, data: JSON.parse(data) as Record<string, unknown> };
}

export function post(harness: Harness, path: string, body?: unknown, headers: Record<string, string> = {}) {
  return harness.app.request(`/v1${path}`, {
    method: 'POST',
    headers: {
      ...AUTHORIZATION,
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
      ...headers,
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
}

export function get(harness: Harness, path: string, headers: Record<string, string> = {}) {
  return harness.app.request(`/v1${path}`, { headers: { ...AUTHORIZATION, ...headers } });
}

export function del(harness: Harness, path: string) {
  return harness.app.request(`/v1${path}`, { method: 'DELETE', headers: AUTHORIZATION });
}

/** Creates a session over HTTP and returns its id plus the initial resource. */
export async function createSession(
  harness: Harness,
  body: Record<string, unknown>,
): Promise<{ id: string; body: Record<string, unknown> }> {
  const response = await post(harness, '/agents/sessions', body);
  if (response.status !== 201) {
    throw new Error(`create failed: ${response.status} ${await response.text()}`);
  }
  const resource = await json<Record<string, unknown>>(response);
  return { id: String(resource.id), body: resource };
}
