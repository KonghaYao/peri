import { ChatRegistry } from "../../worker/chat/registry";
import { fetchApi } from "../../worker/api/router";
import { encodeAuthFrame } from "../../worker/sdk";
import { encodeCommandFrame } from "../../shared/sync";
import { FixtureSocketHost, until } from "./sockets";
import { memoryInstance } from "./wasm";
import type { AcpTransport, Chat, ChatRepository, Env, StartTransport } from "../../worker/types";
import type { JsonRpcNotification } from "../../worker/sdk";
import type { ChatCommand, ChatCommandResult, SyncState } from "../../shared/sync";
import type { AgentResources } from "../../shared/resources";

/** 假宿主计划：一次 `startTransport` 消费一份拷贝，因此 hydrate 与执行轮次互不影响。 */
export interface TransportPlan {
  /** `session/load` 响应前重放的历史通知（顺序即 ACP 重放顺序）。 */
  replay?: JsonRpcNotification[];
  /** 每轮 prompt 产出的根会话文本块。 */
  chunks?: string[];
  blocked?: boolean;
  failure?: unknown;
  closeFailure?: Error;
  closeGate?: Promise<void>;
  startup?: (signal?: AbortSignal) => Promise<void>;
  startupGate?: Promise<void>;
  cancelGate?: Promise<void>;
  cancelFailure?: Error;
  loadFailure?: Error;
  loadConfig?: unknown[];
  tasks?: unknown[];
  /** 把 WasmResources 接到宿主生命周期上（观测面测试用）。 */
  resources?: boolean;
  clock?: () => number;
}

export class FixtureTransport implements AcpTransport {
  readonly calls: { method: string; params?: unknown }[] = [];
  readonly listeners = new Set<(notification: JsonRpcNotification) => void>();
  handler?: (method: string, params: unknown) => unknown | Promise<unknown>;
  closed = false;
  closeRequested = false;
  unsubscribed = false;
  readonly promptStarted: Promise<void>;
  readonly closeStarted: Promise<void>;
  readonly cancelStarted: Promise<void>;
  private sessionId = "";
  private markPromptStarted!: () => void;
  private markCloseStarted!: () => void;
  private markCancelStarted!: () => void;
  private finishPrompt?: (result: { stopReason: string }) => void;
  /** session/load 那一刻的订阅者数量：证明「先订阅再 load」的次序要求。 */
  listenersAtLoad = -1;
  /** 默认助手消息身份：分块共享，模拟一条 ACP 消息的流式内容。 */
  readonly messageId = crypto.randomUUID();

  constructor(private readonly plan: TransportPlan = {}) {
    this.promptStarted = new Promise((resolve) => { this.markPromptStarted = resolve; });
    this.closeStarted = new Promise((resolve) => { this.markCloseStarted = resolve; });
    this.cancelStarted = new Promise((resolve) => { this.markCancelStarted = resolve; });
  }

  async request<Result>(method: string, params?: unknown): Promise<Result> {
    this.calls.push({ method, params });
    if (method === "initialize") return { protocolVersion: 1 } as Result;
    if (method === "session/load") {
      this.sessionId = (params as { sessionId: string }).sessionId;
      this.listenersAtLoad = this.listeners.size;
      // ACP 语义：先重放历史，再响应 load。
      for (const notification of this.plan.replay ?? []) this.emitRaw(notification);
      if (this.plan.loadFailure) throw this.plan.loadFailure;
      return { modes: {}, ...(this.plan.loadConfig ? { configOptions: this.plan.loadConfig } : {}) } as Result;
    }
    if (method === "session/bg-tasks") return { revision: 6, tasks: this.plan.tasks ?? [] } as Result;
    throw new Error(`Unexpected ACP request: ${method}`);
  }

  async sendRequest<Result>(method: string, params?: unknown): Promise<{ response: Promise<Result> }> {
    this.calls.push({ method, params });
    if (method !== "session/prompt") throw new Error(`Unexpected ACP send: ${method}`);
    const response = new Promise<{ stopReason: string }>((resolve, reject) => {
      this.finishPrompt = resolve;
      this.emit("foreign session", { sessionId: "another-session" });
      this.emit("old subagent chunk", { _meta: { "peri.sourceAgentId": "child" } });
      this.emit("subagent chunk", { _meta: { peri: { sourceAgentId: "child" } } });
      for (const chunk of this.plan.chunks ?? ["你好", "🙂"]) this.emit(chunk);
      if (this.plan.failure) reject(this.plan.failure);
      else if (!this.plan.blocked) resolve({ stopReason: "end_turn" });
    });
    this.markPromptStarted();
    return { response: response as Promise<Result> };
  }

  async notify(method: string, params?: unknown): Promise<void> {
    this.calls.push({ method, params });
    if (method !== "session/cancel") throw new Error(`Unexpected ACP notification: ${method}`);
    this.markCancelStarted();
    if (this.plan.cancelFailure) throw this.plan.cancelFailure;
    await this.plan.cancelGate;
    this.finishPrompt?.({ stopReason: "cancelled" });
  }

  async *events(): AsyncIterable<JsonRpcNotification> {}

  subscribe(listener: (notification: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); this.unsubscribed = true; };
  }

  setRequestHandler(handler: (method: string, params: unknown) => unknown | Promise<unknown>): void {
    this.handler = handler;
  }

  async close(): Promise<void> {
    this.closeRequested = true;
    this.markCloseStarted();
    await this.plan.closeGate;
    if (this.plan.closeFailure) throw this.plan.closeFailure;
    this.closed = true;
  }

  /**
   * 根会话文本块（`params` 用于附加会话级字段，例如子代理标记）。
   * 默认共用同一 messageId：与 ACP 实时流一致（同一消息的分块共享 messageId）。
   */
  emit(text: string, params: Record<string, unknown> = {}, messageId = this.messageId): void {
    this.emitRaw(replayChunk(this.sessionId, messageId, text, { params }));
  }

  emitRaw(notification: JsonRpcNotification): void {
    for (const listener of this.listeners) listener(notification);
  }

  finish(stopReason = "end_turn"): void { this.finishPrompt?.({ stopReason }); }

  /** ACP 实时回显的用户输入：与已投递输入同文本，必须被投影去重。 */
  userEcho(text: string): void {
    this.emitRaw(replayChunk(this.sessionId, "actual-acp-input", text, { role: "user" }));
  }

  /** 宿主侧轮次结束事件（SDK 依赖它做任务/排队收敛）。 */
  done(): void {
    this.emitRaw({ jsonrpc: "2.0", method: "peri/agent_event_done",
      params: { sessionId: this.sessionId, stopReason: "end_turn" } });
  }
}

/** 重放/实时文本块；`params` 进入通知级 params，`update` 进入 update 对象。 */
export function replayChunk(sessionId: string, messageId: string, text: string,
  options: { role?: "user" | "assistant" | "thought"; replay?: boolean; params?: Record<string, unknown>;
    update?: Record<string, unknown> } = {}): JsonRpcNotification {
  const kind = options.role === "user" ? "user_message_chunk"
    : options.role === "thought" ? "agent_thought_chunk" : "agent_message_chunk";
  return { jsonrpc: "2.0", method: "session/update", params: {
    sessionId,
    update: { sessionUpdate: kind, messageId, content: { type: "text", text },
      ...(options.replay ? { _meta: { periReplay: true } } : {}), ...options.update },
    ...options.params,
  } };
}

/** 重放的工具结果：大 rawOutput 会被 SDK 外置为载荷引用。 */
export function replayToolCall(sessionId: string, toolCallId: string, rawOutput: string): JsonRpcNotification {
  return { jsonrpc: "2.0", method: "session/update", params: { sessionId,
    update: { sessionUpdate: "tool_call", toolCallId, title: "Read", status: "completed", rawOutput,
      content: [{ type: "text", text: "big result" }], _meta: { periReplay: true } } } };
}

export const promptCommand = (content: string, id = crypto.randomUUID()): ChatCommand =>
  ({ type: "command", id, op: "prompt", content });

export const cancelCommand = (id = crypto.randomUUID()): ChatCommand => ({ type: "command", id, op: "cancel" });

export const interactionCommand = (interactionId: string, answer: unknown, id = crypto.randomUUID()): ChatCommand =>
  ({ type: "command", id, op: "interaction", interactionId, answer });

export const payloadCommand = (refId: string, bytes?: number, id = crypto.randomUUID()): ChatCommand =>
  ({ type: "command", id, op: "payload", refId, ...(bytes === undefined ? {} : { bytes }) });

/** 会话 HTTP/WS 夹具：进程内注册表 + 假宿主 + 内存 socket。 */
export function runtimeFixture(plan: TransportPlan = {}, options: { limit?: number } = {}) {
  const chats = new Map<string, Chat>();
  const repositoryCalls: string[] = [];
  const repository: ChatRepository = {
    async list() { repositoryCalls.push("list"); return structuredClone([...chats.values()]); },
    async get(id) { repositoryCalls.push("get"); return structuredClone(chats.get(id) ?? null); },
    async create(title) {
      repositoryCalls.push("create");
      const chat = { id: `00000000-0000-4000-8000-${String(chats.size + 1).padStart(12, "0")}`,
        title, updatedAt: "2026-10-06T00:00:00.000Z" };
      chats.set(chat.id, structuredClone(chat));
      return structuredClone(chat);
    },
  };
  const env: Env = {
    APP_AUTH_TOKEN: "trusted-app-token",
    PERI_STORAGE_URL: "libsql://fixture.invalid",
    PERI_STORAGE_TOKEN: "private-storage-token",
    MODEL_BASE_URL: "https://model.invalid/v1",
    MODEL_API_KEY: "private-model-token",
    MODEL_ID: "fixture-model",
  };
  const transports: FixtureTransport[] = [];
  const registry = new ChatRegistry(options.limit);
  const host = new FixtureSocketHost(plan.clock);
  const startTransport: StartTransport = async (_env, resources, signal) => {
    await plan.startupGate;
    await plan.startup?.(signal);
    const transport = new FixtureTransport(plan);
    if (plan.resources && resources) {
      resources.attach(memoryInstance());
      resources.ready();
      const close = transport.close.bind(transport);
      transport.close = async () => {
        resources.closing();
        try { await close(); resources.closed(); }
        catch (error) { resources.closeUnconfirmed(); throw error; }
      };
    }
    transports.push(transport);
    return transport;
  };
  const request = (path: string, options: { method?: string; body?: unknown; token?: string | null;
    headers?: Record<string, string> } = {}): Promise<Response> => {
    const headers = new Headers(options.headers);
    const token = options.token === undefined ? env.APP_AUTH_TOKEN! : options.token;
    if (token !== null) headers.set("Authorization", `Bearer ${token}`);
    if (options.body !== undefined) headers.set("Content-Type", "application/json");
    return fetchApi(new Request(`https://app.invalid${path}`, {
      method: options.method ?? "GET", headers,
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
    }), env, repository, { runtimes: registry, socketHost: () => host, startTransport });
  };
  const create = async (title = "Fixture chat"): Promise<Chat> => {
    const response = await request("/api/v2/chats", { method: "POST", body: { title } });
    if (response.status !== 201) throw new Error(`Chat creation failed with HTTP ${response.status}`);
    return await response.json() as Chat;
  };
  /** 打开同步连接并按需发首个鉴权帧；socket 从内存 host 取。 */
  const connect = async (chatId: string, options: { authenticate?: boolean; resume?: Parameters<typeof encodeAuthFrame>[0]["resume"] } = {}) => {
    const response = await request(`/api/v2/chats/${chatId}/sync`, { token: null, headers: { Upgrade: "websocket" } });
    const socket = host.last();
    if (options.authenticate !== false)
      socket.receive(encodeAuthFrame({ type: "auth", token: env.APP_AUTH_TOKEN!, resume: options.resume }));
    return { socket, response };
  };
  /** 连接并等待首个快照（attach 完成）。 */
  const attached = async (chatId: string) => {
    const { socket, response } = await connect(chatId);
    await until(() => socket.frames.length > 0 || !!socket.closed);
    return { socket, response };
  };
  const send = async (socket: ReturnType<FixtureSocketHost["last"]>, command: ChatCommand): Promise<ChatCommandResult> => {
    socket.receive(encodeCommandFrame(command));
    return socket.result(command.id);
  };
  /**
   * 最近一次执行轮次的宿主：复原宿主只做 session/load 后关闭，执行宿主是唯一收到 session/prompt 的。
   */
  const runTransport = async (): Promise<FixtureTransport> => {
    const prompted = () => transports.filter((host) => host.calls.some(({ method }) => method === "session/prompt"));
    await until(() => prompted().length > 0);
    return prompted().at(-1)!;
  };
  const detail = async (chatId: string): Promise<SyncState> =>
    await (await request(`/api/v2/chats/${chatId}`)).json() as SyncState;
  const resources = async (chatId: string): Promise<AgentResources> =>
    await (await request(`/api/v2/chats/${chatId}/resources`)).json() as AgentResources;
  return { env, chats, repository, repositoryCalls, registry, host, transports, request, create, connect, attached,
    send, detail, resources, runTransport, last: () => transports.at(-1)!, drain: () => host.drain() };
}
