import type { ChatExecuteResult, ChatOpenResult, ChatOpenTarget } from '@peri-code/rivet/rivet';
import type { ChatUserAction, InteractionAnswer } from '@peri-code/sdk';
import type { Agent, AgentDraft, AgentPreviewTarget } from '@shared/agent';
import { ApiErrorCode, PUBLIC_ERROR_MESSAGES } from '@shared/api';
import type {
  ChatCancelResponse,
  ChatCloseResponse,
  ChatCreateRequest,
  ChatRuntimeReason,
  ChatRuntimeStatus,
  ChatSendResponse,
  ChatSummary,
} from '@shared/chat';
import { isRetryableRuntime } from '@shared/chat';
import {
  assertSandboxTargetCompatible,
  buildAgentTarget,
  injectSandboxTarget,
  resolveAgentProvider,
} from '../agents/open-target';
import type { AgentsRepository } from '../agents/repository';
import type { ServerEnv } from '../env';
import { AppError, diagnosticCode } from '../errors';
import { ProvidersRepository } from '../providers/repository';
import { ProviderService } from '../providers/service';
import type { SandboxCoordinator } from '../sandbox/coordinator';
import type { ChatActorPort, ChatEngine } from './engine';
import type { ChatsRepository } from './repository';
import { ChatStreamHub } from './stream';

const MAX_RECEIPTS_PER_CHAT = 200;

interface Receipt {
  readonly text: string;
  readonly response: ChatSendResponse;
}

interface Inflight {
  readonly text: string;
  readonly promise: Promise<ChatSendResponse>;
}

function errorCode(error: unknown): string {
  return diagnosticCode(error);
}

function asDraft(agent: Agent): AgentDraft {
  return { name: agent.name, description: agent.description, config: agent.config };
}

/**
 * Sandbox holder selection for one chat. `null` means the chat runs without an injected
 * environment; the default (no policy) always uses the chat id, which is today's behavior.
 */
export interface ChatEnvironmentPolicy {
  holderFor(chatId: string): string | null;
}

/** Optional creation seams used by the platform session surface; every field defaults away. */
export interface ChatCreateOptions {
  /** Caller-owned chat id, so a platform session id and its chat key stay identical. */
  readonly chatId?: string;
  /** Effective agent configuration resolved by the caller (inline agent snapshots). */
  readonly draft?: AgentDraft;
  readonly policy?: ChatEnvironmentPolicy;
}

export interface ChatServiceOptions {
  readonly env: ServerEnv;
  readonly engine: ChatEngine;
  readonly chats: ChatsRepository;
  readonly agents: AgentsRepository;
  readonly providers?: ProviderService;
  readonly sandbox?: SandboxCoordinator;
  readonly generateId?: () => string;
  /** Last-resort draft source when no in-memory run exists yet (durable inline agents). */
  readonly draftFor?: (chatId: string) => AgentDraft | null;
}

/**
 * Host-side chat orchestration. 首次操作在 per-chat 队列中同步冻结目标并登记引用；
 * 后续操作仅复用当前运行目标，本地 SQLite 不持久化执行快照。
 *
 * Sandbox readiness is acquired before `open` so the target carries the real MCP URL. A confirmed
 * Chat close releases the holder; an unconfirmed one keeps the environment alive for retry.
 */
export class ChatService {
  readonly providers: ProviderService;
  readonly #runs = new Map<
    string,
    { readonly draft: AgentDraft; readonly base: AgentPreviewTarget; target: AgentPreviewTarget | null }
  >();
  readonly #env: ServerEnv;
  readonly #engine: ChatEngine;
  readonly #chats: ChatsRepository;
  readonly #agents: AgentsRepository;
  readonly #sandbox: SandboxCoordinator | null;
  readonly #generateId: () => string;
  readonly #draftFor: ((chatId: string) => AgentDraft | null) | null;
  readonly #drafts = new Map<string, AgentDraft>();
  readonly #policies = new Map<string, ChatEnvironmentPolicy>();
  readonly #hubs = new Map<string, ChatStreamHub>();
  readonly #receipts = new Map<string, Map<string, Receipt>>();
  readonly #inflight = new Map<string, Map<string, Inflight>>();
  readonly #operations = new Map<string, Promise<void>>();
  readonly #closing = new Set<string>();
  readonly #closed = new Set<string>();
  readonly #active = new Set<string>();
  readonly #closeFailures = new Map<string, unknown>();
  readonly #closeRequests = new Map<string, Promise<ChatCloseResponse>>();
  #stopping = false;

  constructor(options: ChatServiceOptions) {
    this.#env = options.env;
    this.#engine = options.engine;
    this.#chats = options.chats;
    this.#agents = options.agents;
    this.providers = options.providers ?? new ProviderService(new ProvidersRepository(options.agents.db));
    this.#sandbox = options.sandbox ?? null;
    this.#generateId = options.generateId ?? (() => crypto.randomUUID());
    this.#draftFor = options.draftFor ?? null;
  }

  get runtime(): ChatRuntimeStatus {
    return this.#engine.status;
  }

  list(agentId?: string): ChatSummary[] {
    return this.#chats.list(agentId).map((chat) => this.#withSandbox(chat));
  }

  async create(request: ChatCreateRequest, options: ChatCreateOptions = {}): Promise<ChatSummary> {
    this.#assertAccepting();
    await this.#ensureRuntime();
    this.#assertAccepting();
    const agent = this.#agents.get(request.agentId);
    // A caller-supplied draft (inline platform agent) is authoritative: reserved agent rows are
    // hidden from the domain repository, so its presence check cannot apply to them.
    if (!agent && options.draft === undefined) throw new AppError('not_found', 'Agent not found');
    const chatId = options.chatId ?? this.#generateId();
    // Reserve the caller-owned seams before the row exists: `#freeze` runs inside the per-chat
    // queue and must already see the effective draft and environment policy.
    if (options.draft) this.#drafts.set(chatId, structuredClone(options.draft));
    if (options.policy) this.#policies.set(chatId, options.policy);
    let created: ChatSummary;
    try {
      created = this.#chats.create(chatId, agent?.id ?? request.agentId);
    } catch (error) {
      this.#drafts.delete(chatId);
      this.#policies.delete(chatId);
      throw error;
    }
    const chat = this.#chats.get(chatId);
    if (!chat) throw new AppError('internal_error', 'Chat row disappeared after creation');

    return this.#queue(chatId, async () => {
      let openAttempted = false;
      try {
        this.#freeze(chatId);
        const target = await this.#target(chatId);
        openAttempted = true;
        await this.#open(chat, target);
        return this.#withSandbox(created);
      } catch (error) {
        await this.#rollbackCreate(chatId, openAttempted);
        this.#normalize(error);
      }
    });
  }

  async send(chatId: string, requestId: string, text: string): Promise<ChatSendResponse> {
    this.#assertAccepting(chatId);
    const cached = this.#receipts.get(chatId)?.get(requestId);
    if (cached) {
      if (cached.text !== text) throw this.#conflict(requestId);
      return cached.response;
    }
    const inflight = this.#inflight.get(chatId)?.get(requestId);
    if (inflight) {
      if (inflight.text !== text) throw this.#conflict(requestId);
      return inflight.promise;
    }

    const promise = this.#queue(chatId, () => this.#send(chatId, requestId, text));
    let perChat = this.#inflight.get(chatId);
    if (!perChat) {
      perChat = new Map();
      this.#inflight.set(chatId, perChat);
    }
    perChat.set(requestId, { text, promise });
    try {
      return await promise;
    } finally {
      perChat.delete(requestId);
    }
  }

  async #send(chatId: string, requestId: string, text: string): Promise<ChatSendResponse> {
    const chat = this.#chats.get(chatId);
    if (!chat) throw new AppError('not_found', 'Chat not found');
    // Runtime first: a Store/engine outage must not freeze a target for a request that cannot run.
    await this.#ensureRuntime();
    this.#freeze(chatId);
    const target = await this.#target(chatId);
    const opened = await this.#open(chat, target);
    try {
      await this.#ensureSession(chat, opened.actor, opened.result);
    } catch (error) {
      // A refused session is the actor's public outcome; map it instead of surfacing a bare 500.
      this.#normalize(error);
    }

    let result: ChatExecuteResult;
    try {
      result = await opened.actor.execute({
        type: 'send',
        requestId,
        payload: { text },
      } satisfies ChatUserAction);
    } catch (error) {
      this.#normalize(error);
    }
    if (result.type !== 'send') {
      throw new AppError('internal_error', 'Chat actor returned an unexpected send result');
    }

    this.#recordSession(chatId, result.target.sessionId);
    const response: ChatSendResponse = {
      requestId: result.requestId,
      inputId: result.inputId,
      sessionId: result.target.sessionId,
      bindingRevision: result.target.bindingRevision,
    };
    this.#storeReceipt(chatId, requestId, text, response);
    return response;
  }

  cancel(chatId: string, requestId?: string): Promise<ChatCancelResponse> {
    this.#assertAccepting(chatId);
    return this.#queue(chatId, () => this.#cancel(chatId, requestId));
  }

  async #cancel(chatId: string, requestId?: string): Promise<ChatCancelResponse> {
    const chat = this.#chats.get(chatId);
    if (!chat) throw new AppError('not_found', 'Chat not found');
    await this.#ensureRuntime();
    this.#freeze(chatId);
    const { actor } = await this.#open(chat, await this.#target(chatId));
    let result: ChatExecuteResult;
    try {
      result = await actor.execute({
        type: 'cancel',
        requestId: requestId ?? this.#generateId(),
        payload: {},
      } satisfies ChatUserAction);
    } catch (error) {
      this.#normalize(error);
    }
    if (result.type !== 'cancel') {
      throw new AppError('internal_error', 'Chat actor returned an unexpected cancel result');
    }
    return { requestId: result.result.requestId, status: result.result.status };
  }

  /** Answers one pending interaction; the interaction id comes from the session projection. */
  answer(chatId: string, interactionId: string, answer: InteractionAnswer): Promise<void> {
    this.#assertAccepting(chatId);
    return this.#queue(chatId, () => this.#answer(chatId, interactionId, answer));
  }

  async #answer(chatId: string, interactionId: string, answer: InteractionAnswer): Promise<void> {
    const chat = this.#chats.get(chatId);
    if (!chat) throw new AppError('not_found', 'Chat not found');
    await this.#ensureRuntime();
    this.#freeze(chatId);
    const { actor } = await this.#open(chat, await this.#target(chatId));
    let result: ChatExecuteResult;
    try {
      result = await actor.execute({
        type: 'answer',
        requestId: this.#generateId(),
        payload: { interactionId, answer },
      } satisfies ChatUserAction);
    } catch (error) {
      this.#normalize(error);
    }
    if (result.type !== 'answer') {
      throw new AppError('internal_error', 'Chat actor returned an unexpected answer result');
    }
  }

  close(chatId: string): Promise<ChatCloseResponse> {
    const pending = this.#closeRequests.get(chatId);
    if (pending) return pending;
    if (!this.#chats.get(chatId)) throw new AppError('not_found', 'Chat not found');
    this.#closing.add(chatId);
    const close = this.#queue(chatId, () => this.#close(chatId));
    this.#closeRequests.set(chatId, close);
    const clear = () => this.#closeRequests.delete(chatId);
    void close.then(clear, clear);
    return close;
  }

  async #close(chatId: string): Promise<ChatCloseResponse> {
    if (!this.#closed.has(chatId)) {
      try {
        await this.#ensureRuntime();
        await this.#engine.actor(chatId).close();
        this.#closed.add(chatId);
        this.#closeFailures.delete(chatId);
      } catch (error) {
        this.#closeFailures.set(chatId, error);
        console.error('[agent-builder] chat close incomplete', { code: errorCode(error) });
        return { status: 'close_incomplete', errors: ['chat_close_failed'] };
      }
    }

    await this.#disposeHub(chatId);
    this.#receipts.delete(chatId);
    this.#inflight.delete(chatId);
    const sandbox = await this.#sandbox?.release(this.#holderFor(chatId) ?? chatId);
    if (sandbox !== undefined && !sandbox.released) {
      return { status: 'close_incomplete', errors: sandbox.errors };
    }
    this.#releaseTarget(chatId);
    // 资源已确认释放：正常入口可以按 Store 历史建立新运行；未确认的 close 继续留在 #closing。
    this.#closing.delete(chatId);
    return { status: 'closed', errors: [] };
  }

  /**
   * Removes the host mapping only after the actor and its sandbox holder are confirmed closed.
   * The ACP history remains in the Store. An unconfirmed close keeps the row so it can be retried.
   */
  async remove(chatId: string): Promise<void> {
    if (!this.#chats.get(chatId)) throw new AppError('not_found', 'Chat not found');
    // 未配置且从未取得 holder 的历史映射可以离线删除；活跃 Chat 必须确认关闭。
    if (
      this.#engine.status.mode !== 'wasm' &&
      (this.#sandbox === null ||
        ['disabled', 'closed'].includes(this.#sandbox.state(this.#holderFor(chatId) ?? chatId).state)) &&
      !this.#active.has(chatId) &&
      !this.#runs.has(chatId) &&
      !this.#operations.has(chatId)
    ) {
      this.#closing.add(chatId);
      await this.#queue(chatId, () => this.#disposeHub(chatId));
    } else {
      const result = await this.close(chatId);
      if (result.status !== 'closed') {
        throw new AppError('close_incomplete', 'Chat and sandbox were not confirmed closed', {
          details: { actorClosed: this.#closed.has(chatId), sandboxErrors: result.errors },
          retryable: true,
          cause: this.#closeFailures.get(chatId),
        });
      }
    }
    this.#chats.delete(chatId);
    this.#closing.delete(chatId);
    this.#closed.delete(chatId);
    this.#active.delete(chatId);
    this.#closeFailures.delete(chatId);
  }

  /** Starts the engine and the projection before the SSE response is committed. */
  prepareStream(chatId: string): Promise<ChatStreamHub> {
    this.#assertAccepting(chatId);
    return this.#queue(chatId, () => this.#prepareStream(chatId));
  }

  async #prepareStream(chatId: string): Promise<ChatStreamHub> {
    const chat = this.#chats.get(chatId);
    if (!chat) throw new AppError('not_found', 'Chat not found');
    await this.#ensureRuntime();
    this.#freeze(chatId);
    const opened = await this.#open(chat, await this.#target(chatId));
    // A fresh chat is bound only after `create_new_session`; starting the projection first would
    // subscribe against a null binding and the first model turn would never reach the stream.
    try {
      await this.#ensureSession(chat, opened.actor, opened.result);
    } catch (error) {
      this.#normalize(error);
    }

    const existing = this.#hubs.get(chatId);
    if (existing) {
      await existing.ready();
      return existing;
    }

    this.#active.add(chatId);
    const hub = new ChatStreamHub(chatId, this.#engine.actor(chatId));
    this.#hubs.set(chatId, hub);
    try {
      await hub.start();
    } catch (error) {
      this.#hubs.delete(chatId);
      await hub.dispose();
      this.#normalize(error);
    }
    return hub;
  }

  /**
   * Closes every Chat before its sandbox holder is released. `complete:false` means at least one
   * actor (and therefore its environment) was not confirmed closed; shutdown must still report it
   * and keep the process exit code non-zero instead of claiming cleanup.
   */
  async closeAllChats(): Promise<{ complete: boolean; errors: string[] }> {
    this.#stopping = true;
    const results = await Promise.all(
      this.#chats.list().map(async (chat) => {
        try {
          return await this.close(chat.chatId);
        } catch (error) {
          console.error('[agent-builder] chat shutdown incomplete', { code: errorCode(error) });
          return { status: 'close_incomplete', errors: ['chat_close_failed'] } as const;
        }
      }),
    );
    return {
      complete: results.every((result) => result.status === 'closed'),
      errors: results.flatMap((result) => result.errors),
    };
  }

  /** 完整关闭未确认时保留 engine，不能绕过持有者释放全局资源。 */
  async shutdown(): Promise<void> {
    const result = await this.closeAllChats();
    if (!result.complete) throw new AppError('close_incomplete', PUBLIC_ERROR_MESSAGES.close_incomplete);
    await this.#engine.shutdown();
  }

  async #open(
    chat: ChatSummary,
    frozen: AgentPreviewTarget,
  ): Promise<{ actor: ChatActorPort; result: ChatOpenResult }> {
    await this.#engine.start();
    const actor = this.#engine.actor(chat.chatId);
    const target = structuredClone(frozen) as unknown as ChatOpenTarget;
    this.#active.add(chat.chatId);
    try {
      const result = await this.#openWithRetry(actor, target);
      // 新激活已建立：之后的 close 必须再次关闭它，而不是沿用上一次 close 的结论。
      this.#closed.delete(chat.chatId);
      return { actor, result };
    } catch (error) {
      this.#normalize(error);
    }
  }

  #freeze(chatId: string): void {
    if (this.#runs.has(chatId)) return;
    // Caller-resolved drafts (inline platform agents) and durable snapshots win over the chat row
    // agent; without either, the stored agent row is the only source and behavior is unchanged.
    const override = this.#drafts.get(chatId) ?? this.#draftFor?.(chatId) ?? null;
    const draft = structuredClone(override ?? this.#storedDraft(chatId));
    const resolved = resolveAgentProvider(draft, this.providers);
    if (!resolved || resolved.providerId === null)
      throw new AppError('not_ready', 'Select a shared Provider and fill its SQLite API key in Providers', {
        details: { reason: 'provider_missing' },
      });
    if (resolved.apiKey === undefined)
      throw new AppError('not_ready', 'Fill this Provider SQLite API key in Providers', {
        details: { reason: 'api_key_missing' },
      });
    if (this.#sandbox?.enabled === true && this.#holderFor(chatId) !== null) {
      assertSandboxTargetCompatible(draft);
    }
    const base = structuredClone(buildAgentTarget(this.#env, draft, chatId, { resolved }));
    this.#runs.set(chatId, { draft, base, target: null });
    if (resolved.providerId !== null) this.providers.retain(chatId, resolved.providerId);
  }

  /** Only used when no caller or durable draft exists, i.e. the unchanged `/api/chats` path. */
  #storedDraft(chatId: string): AgentDraft {
    const chat = this.#chats.getWithAgent(chatId);
    if (!chat) throw new AppError('not_found', 'Chat not found');
    return asDraft(chat.agent);
  }

  /** The policy may opt out of an environment entirely; otherwise the chat id is the holder. */
  #holderFor(chatId: string): string | null {
    const policy = this.#policies.get(chatId);
    return policy === undefined ? chatId : policy.holderFor(chatId);
  }

  async #target(chatId: string): Promise<AgentPreviewTarget> {
    const run = this.#runs.get(chatId);
    if (!run) throw new AppError('internal_error', 'Missing frozen target');
    if (run.target !== null) return run.target;
    const holder = this.#holderFor(chatId);
    const injection = holder === null ? undefined : await this.#sandbox?.acquire(holder);
    const target = injection
      ? injectSandboxTarget(run.base, run.draft, injection)
      : structuredClone(run.base);
    run.target = target;
    return target;
  }

  #releaseTarget(chatId: string): void {
    this.#runs.delete(chatId);
    this.#drafts.delete(chatId);
    this.#policies.delete(chatId);
    this.#active.delete(chatId);
    this.providers.release(chatId);
  }

  #withSandbox(chat: ChatSummary): ChatSummary {
    return {
      ...chat,
      sandbox: this.#sandbox?.state(chat.chatId) ?? { state: 'disabled', reason: null },
    };
  }

  /**
   * Releases exactly the progress made by a failed create. If the release itself is unconfirmed the
   * row is kept so the holder is visible and retryable; deleting it would orphan the environment.
   */
  async #rollbackCreate(chatId: string, openAttempted: boolean): Promise<void> {
    if (openAttempted) {
      // `actor()` throwing means the engine never produced a handle, so no activation exists and
      // the row can be removed even while the runtime is unavailable.
      const actor = this.#actorOrNull(chatId);
      if (actor !== null) {
        try {
          await actor.close();
        } catch (error) {
          // The failed activation may still be using the environment; keep the row and holder so the
          // user can retry a real close instead of orphaning a running Chat.
          console.error('[agent-builder] failed create could not close its actor', {
            code: errorCode(error),
          });
          return;
        }
      }
    }
    if (this.#sandbox === null) {
      this.#releaseTarget(chatId);
      this.#chats.delete(chatId);
      return;
    }
    const released = await this.#sandbox.release(this.#holderFor(chatId) ?? chatId);
    if (released.released) {
      this.#releaseTarget(chatId);
      this.#chats.delete(chatId);
      return;
    }
    console.error('[agent-builder] failed chat create left an unconfirmed sandbox holder', {
      errors: released.errors,
    });
  }

  #actorOrNull(chatId: string): ChatActorPort | null {
    try {
      return this.#engine.actor(chatId);
    } catch {
      return null;
    }
  }

  async #openWithRetry(actor: ChatActorPort, target: ChatOpenTarget): Promise<ChatOpenResult> {
    try {
      return await actor.open(target);
    } catch (error) {
      if (errorCode(error) !== 'binding_conflict') throw error;
      await actor.close();
      return actor.open(target);
    }
  }

  /**
   * Loads the stored session first. Only a typed `not_found` (history pruned) may fall back to a
   * new session; an actor/Store failure must surface instead of silently discarding history.
   */
  async #ensureSession(chat: ChatSummary, actor: ChatActorPort, opened: ChatOpenResult): Promise<void> {
    const current = opened.binding?.sessionId ?? null;
    if (chat.sessionId !== null && chat.sessionId !== current) {
      try {
        const changed = await actor.execute({
          type: 'change_session',
          requestId: this.#generateId(),
          payload: { sessionId: chat.sessionId },
        } satisfies ChatUserAction);
        if (changed.type === 'change_session') {
          this.#recordSession(chat.chatId, changed.result.sessionId);
          return;
        }
      } catch (error) {
        const code = errorCode(error);
        if (code !== 'not_found') {
          throw error;
        }
        console.warn('[agent-builder] stored session is missing; starting a new one');
      }
    }
    if (current !== null) {
      if (chat.sessionId !== current) this.#recordSession(chat.chatId, current);
      return;
    }

    const created = await actor.execute({
      type: 'create_new_session',
      requestId: this.#generateId(),
      payload: {},
    } satisfies ChatUserAction);
    if (created.type === 'create_new_session') {
      this.#recordSession(chat.chatId, created.result.sessionId);
    }
  }

  #recordSession(chatId: string, sessionId: string): void {
    this.#chats.setSessionId(chatId, sessionId);
  }

  #storeReceipt(chatId: string, requestId: string, text: string, response: ChatSendResponse): void {
    let perChat = this.#receipts.get(chatId);
    if (!perChat) {
      perChat = new Map();
      this.#receipts.set(chatId, perChat);
    }
    perChat.set(requestId, { text, response });
    while (perChat.size > MAX_RECEIPTS_PER_CHAT) {
      const oldest = perChat.keys().next().value;
      if (oldest === undefined) break;
      perChat.delete(oldest);
    }
  }

  #conflict(requestId: string): AppError {
    return new AppError('request_conflict', 'requestId was already used with different text', {
      details: { requestId },
    });
  }

  /**
   * Runtime-guarded entry: a Store that was unreachable at boot may recover, so recoverable
   * engines get one TTL-gated probe before the request decides whether chats can run.
   */
  #assertAccepting(chatId?: string): void {
    if (this.#stopping || (chatId !== undefined && this.#closing.has(chatId))) {
      throw new AppError('closed', 'Chat is closing or closed');
    }
  }

  // 同一 Chat 的 open/execute 与 close 共用队列，不能在 await 期间释放它仍在使用的环境。
  #queue<T>(chatId: string, operation: () => Promise<T>): Promise<T> {
    const previous = this.#operations.get(chatId) ?? Promise.resolve();
    const result = previous.then(operation);
    const settled = result.then(
      () => undefined,
      () => undefined,
    );
    this.#operations.set(chatId, settled);
    void settled.then(() => {
      if (this.#operations.get(chatId) === settled) this.#operations.delete(chatId);
    });
    return result;
  }

  async #ensureRuntime(): Promise<void> {
    try {
      await this.#engine.ensureReady?.();
      this.#assertRuntime();
      await this.#engine.start();
    } catch (error) {
      // Every guarded entry point goes through here, so a raw start/probe failure is typed 503
      // exactly once instead of leaking as an untyped 500 from `create` or `send`.
      this.#normalize(error);
    }
  }

  #assertRuntime(): void {
    if (this.#engine.status.mode !== 'wasm') {
      const reasons: ChatRuntimeReason[] = [...this.#engine.status.reasons];
      throw new AppError('engine_unavailable', 'Chat runtime is not configured', {
        details: { reasons },
        retryable: isRetryableRuntime(reasons),
      });
    }
  }

  /**
   * Narrow error normalization for the chat entry points.
   *
   * A typed `AppError` already owns its status and retryability (for example the SSE replay 503).
   * The remaining failures are transport errors from a dead Store/engine child: they carry no
   * trusted API code, so they must become the runtime's own 503 instead of an untyped 500. While
   * the engine reports `ready`, unknown failures keep the 500 fallback so real defects stay visible.
   */
  #normalize(error: unknown): never {
    if (error instanceof AppError) throw error;
    const parsed = ApiErrorCode.safeParse(errorCode(error));
    if (parsed.success && parsed.data === 'engine_unavailable') throw this.#engineUnavailable(error);
    if (!parsed.success || parsed.data === 'internal_error') {
      if (!this.#runtimeReady()) throw this.#engineUnavailable(error);
      throw new AppError('internal_error', PUBLIC_ERROR_MESSAGES.internal_error, { cause: error });
    }
    throw new AppError(parsed.data, PUBLIC_ERROR_MESSAGES[parsed.data], {
      retryable: parsed.data === 'session_creation_failed',
      cause: error,
    });
  }

  #engineUnavailable(cause: unknown): AppError {
    const reasons = this.#runtimeReasons();
    return new AppError('engine_unavailable', PUBLIC_ERROR_MESSAGES.engine_unavailable, {
      details: { reasons },
      retryable: isRetryableRuntime(reasons),
      cause,
    });
  }

  /** The engine's observed health; `wasm` only states the configured mode. */
  #runtimeReady(): boolean {
    return this.#engine.health().status === 'ready';
  }

  #runtimeReasons(): ChatRuntimeReason[] {
    return [...(this.#engine.health().reasons ?? this.#engine.status.reasons)];
  }

  async #disposeHub(chatId: string): Promise<void> {
    const hub = this.#hubs.get(chatId);
    if (!hub) return;
    this.#hubs.delete(chatId);
    await hub.close(null);
    await hub.dispose();
  }
}
