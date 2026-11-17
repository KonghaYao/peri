import type {
  ChatExecuteResult,
  ChatFactsResult,
  ChatOpenResult,
  ChatOpenTarget,
} from '@peri-code/rivet/rivet';
import type { ChatFactEnvelope, ChatUserAction } from '@peri-code/sdk';
import type { EngineHealth } from '@shared/api';
import type { ChatRuntimeStatus } from '@shared/chat';
import type { ChatActorPort, ChatEngine } from '../../../server/chats/engine';
import type { SandboxActorPort } from '../../../server/sandbox/coordinator';

export const BINDING = {
  chatId: 'chat-1',
  sessionId: 'session-1',
  bindingRevision: 1,
} as const;

export function envelope(sequence: number, fact: ChatFactEnvelope['fact']): ChatFactEnvelope {
  return {
    chatId: BINDING.chatId,
    sessionId: BINDING.sessionId,
    bindingRevision: BINDING.bindingRevision,
    sequence,
    fact,
  };
}

export function userInput(inputId: string, text: string, sequence = 1): ChatFactEnvelope {
  return envelope(sequence, { kind: 'input-delivered', inputId, text });
}

export function agentChunk(messageId: string, text: string, sequence: number): ChatFactEnvelope {
  return envelope(sequence, {
    kind: 'notice',
    notification: {
      jsonrpc: '2.0',
      method: 'session/update',
      params: {
        sessionId: BINDING.sessionId,
        update: {
          sessionUpdate: 'agent_message_chunk',
          messageId,
          content: { type: 'text', text },
        },
      },
    } as never,
  });
}

export class FakeActor implements ChatActorPort {
  readonly opened: ChatOpenTarget[] = [];
  readonly actions: ChatUserAction[] = [];
  readonly listeners = new Set<(envelope: ChatFactEnvelope) => void>();
  factsResult: ChatFactsResult = { binding: null, envelopes: [], overflow: false };
  bound = false;
  closed = false;
  failOpenWith: Error | null = null;
  failOpenAlwaysWith: Error | null = null;
  failChangeSession = false;
  failChangeSessionWith: Error | null = null;
  failCreateSessionWith: Error | null = null;
  failCloseWith: Error | null = null;

  async open(target: ChatOpenTarget): Promise<ChatOpenResult> {
    if (this.failOpenAlwaysWith) throw this.failOpenAlwaysWith;
    if (this.failOpenWith) {
      const error = this.failOpenWith;
      this.failOpenWith = null;
      throw error;
    }
    this.opened.push(target);
    return {
      chatId: target.chatId,
      workspace: target.workspace,
      binding: this.bound ? { ...BINDING, chatId: target.chatId } : null,
      reused: this.opened.length > 1,
    };
  }

  async execute(action: ChatUserAction): Promise<ChatExecuteResult> {
    this.actions.push(action);
    switch (action.type) {
      case 'create_new_session':
        if (this.failCreateSessionWith) {
          const error = this.failCreateSessionWith;
          this.failCreateSessionWith = null;
          throw error;
        }
        this.bound = true;
        return {
          type: 'create_new_session',
          result: { ...BINDING, requestId: action.requestId },
        };
      case 'change_session':
        if (this.failChangeSessionWith) throw this.failChangeSessionWith;
        if (this.failChangeSession) throw Object.assign(new Error('session missing'), { code: 'not_found' });
        this.bound = true;
        return {
          type: 'change_session',
          result: {
            chatId: BINDING.chatId,
            sessionId: action.payload.sessionId,
            bindingRevision: 2,
            requestId: action.requestId,
          },
        };
      case 'send':
        return {
          type: 'send',
          requestId: action.requestId,
          inputId: `input-${this.actions.length}`,
          target: { ...BINDING },
        };
      case 'cancel':
        return { type: 'cancel', result: { requestId: action.requestId, status: 'requested' } };
      default:
        throw Object.assign(new Error(`unsupported action ${action.type}`), {
          code: 'invalid_action',
        });
    }
  }

  async facts(): Promise<ChatFactsResult> {
    return this.factsResult;
  }

  async close(): Promise<{ closed: true }> {
    if (this.failCloseWith) throw this.failCloseWith;
    this.closed = true;
    // A confirmed close drops the activation; the next open starts a new run from Store history.
    this.bound = false;
    this.factsResult = { binding: null, envelopes: [], overflow: false };
    return { closed: true };
  }

  async onFact(listener: (envelope: ChatFactEnvelope) => void): Promise<() => Promise<void>> {
    this.listeners.add(listener);
    return async () => {
      this.listeners.delete(listener);
    };
  }

  emit(item: ChatFactEnvelope): void {
    for (const listener of [...this.listeners]) listener(item);
  }
}

export class FakeSandboxActor implements SandboxActorPort {
  readonly uri: string;
  readonly acquireCalls: Array<{ holder: string; credential: string }> = [];
  readonly releaseCalls: Array<{ holder: string; credential: string }> = [];
  actorId = 'actor-1';
  failAcquire: { code: string; message: string } | null = null;
  failRelease: { code: string; message: string } | null = null;

  constructor(uri: string) {
    this.uri = uri;
  }

  async acquire(holder: string, credential: string) {
    this.acquireCalls.push({ holder, credential });
    if (this.failAcquire) {
      const error = this.failAcquire;
      this.failAcquire = null;
      return {
        ok: false as const,
        error: { name: 'PeriError' as const, phase: 'runtime' as const, retryable: true, ...error } as never,
      };
    }
    return { ok: true as const, value: { workspaceUri: this.uri, state: 'ready' as const } };
  }

  async release(holder: string, credential: string) {
    this.releaseCalls.push({ holder, credential });
    if (this.failRelease) {
      const error = this.failRelease;
      this.failRelease = null;
      return {
        ok: false as const,
        error: { name: 'PeriError' as const, phase: 'runtime' as const, retryable: true, ...error } as never,
      };
    }
    return { ok: true as const, value: { released: true as const } };
  }

  async resolve(): Promise<string> {
    return this.actorId;
  }
}

export class FakeEngine implements ChatEngine {
  readonly status: ChatRuntimeStatus;
  readonly actors = new Map<string, FakeActor>();
  readonly sandboxes = new Map<string, FakeSandboxActor>();
  readonly endpoint = 'http://127.0.0.1:8961';
  started = 0;
  shutdownCalls = 0;

  constructor(status: ChatRuntimeStatus = { mode: 'wasm', reasons: [] }) {
    this.status = status;
  }

  start(): Promise<void> {
    this.started += 1;
    return Promise.resolve();
  }

  health(): EngineHealth {
    return { mode: 'embedded', status: 'ready' };
  }

  actor(chatId: string): ChatActorPort {
    let actor = this.actors.get(chatId);
    if (!actor) {
      actor = new FakeActor();
      this.actors.set(chatId, actor);
    }
    return actor;
  }

  sandbox(workspaceUri: string): FakeSandboxActor {
    let actor = this.sandboxes.get(workspaceUri);
    if (!actor) {
      actor = new FakeSandboxActor(workspaceUri);
      this.sandboxes.set(workspaceUri, actor);
    }
    return actor;
  }

  shutdown(): Promise<void> {
    this.shutdownCalls += 1;
    return Promise.resolve();
  }
}
