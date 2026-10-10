import { describe, expect, test } from "bun:test";
import type { JsonRpcNotification } from "../worker/sdk";
import { ChatSessionCore } from "../worker/chat/session";
import { ChatRecordStore, type RecordStorage } from "../worker/chat/record";
import { fetchApi } from "../worker/api/router";
import type { AcpTransport, Chat, ChatRepository, Env, SessionState } from "../worker/types";

/** P4 交互往返：反向请求 → SDK 投影 → view/WS → 鉴权命令 → live responder → 原 ACP response。 */

const chatToken = "trusted-app-token";

class FixtureTransport implements AcpTransport {
  readonly calls: { method: string; params?: unknown }[] = [];
  readonly promptStarted: Promise<void>;
  readonly listeners = new Set<(notification: JsonRpcNotification) => void>();
  handler?: (method: string, params: unknown) => unknown | Promise<unknown>;
  closed = false;
  private markPromptStarted!: () => void;
  private finishPrompt!: (result: { stopReason: string }) => void;
  private sessionId = "";

  constructor(private readonly plan: { blocked?: boolean; loadConfig?: unknown[]; tasks?: unknown[] } = {}) {
    this.promptStarted = new Promise((resolve) => { this.markPromptStarted = resolve; });
  }

  async request<Result>(method: string, params?: unknown): Promise<Result> {
    this.calls.push({ method, params });
    if (method === "initialize") return { protocolVersion: 1 } as Result;
    if (method === "session/load") {
      this.sessionId = (params as { sessionId: string }).sessionId;
      return { modes: {}, ...(this.plan.loadConfig ? { configOptions: this.plan.loadConfig } : {}) } as Result;
    }
    if (method === "session/bg-tasks") return { revision: 6, tasks: this.plan.tasks ?? [] } as Result;
    throw new Error(`Unexpected ACP request: ${method}`);
  }

  async sendRequest<Result>(method: string, params?: unknown): Promise<{ response: Promise<Result> }> {
    this.calls.push({ method, params });
    if (method !== "session/prompt") throw new Error(`Unexpected ACP send: ${method}`);
    const response = new Promise<{ stopReason: string }>((resolve) => {
      this.finishPrompt = resolve;
      this.markPromptStarted();
      if (!this.plan.blocked) resolve({ stopReason: "end_turn" });
    });
    return { response: response as Promise<Result> };
  }

  async notify(method: string): Promise<void> {
    this.calls.push({ method });
    if (method !== "session/cancel") throw new Error(`Unexpected ACP notification: ${method}`);
    this.finishPrompt?.({ stopReason: "cancelled" });
  }

  async *events(): AsyncIterable<JsonRpcNotification> {}

  subscribe(listener: (notification: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  setRequestHandler(handler: (method: string, params: unknown) => unknown | Promise<unknown>): void {
    this.handler = handler;
  }

  async close(): Promise<void> { this.closed = true; }

  emitRaw(notification: JsonRpcNotification): void { for (const listener of this.listeners) listener(notification); }
}

async function until2(predicate: () => Promise<boolean>, budgetMs = 2_000): Promise<void> {
  const deadline = Date.now() + budgetMs;
  while (!(await predicate())) {
    if (Date.now() > deadline) throw new Error("condition not reached in time");
    await Bun.sleep(2);
  }
}

const versionedPair = { chat: 3, session: 2 } as const;

function storeFor(records: Map<string, Map<string, unknown>>, chat: Chat): ChatRecordStore {
  const bucket = (): Map<string, unknown> => {
    let map = records.get(chat.id);
    if (!map) { map = new Map(); records.set(chat.id, map); }
    return map;
  };
  const storage: RecordStorage = {
    async get<Value>(key: string) { return structuredClone(records.get(chat.id)?.get(key)) as Value | undefined; },
    async put(entries: Record<string, unknown>) { for (const [key, value] of Object.entries(entries)) bucket().set(key, structuredClone(value)); },
    async delete(keys: string[]) { let removed = false; for (const key of keys) removed = bucket().delete(key) || removed; return removed; },
    async list(options: { prefix?: string } = {}) {
      const result = new Map<string, unknown>();
      for (const [key, value] of [...bucket()].sort(([left], [right]) => left.localeCompare(right))) {
        if (options.prefix && !key.startsWith(options.prefix)) continue;
        result.set(key, structuredClone(value));
      }
      return result;
    },
  };
  return new ChatRecordStore(storage);
}

async function fixture(plan: { blocked?: boolean; loadConfig?: unknown[]; tasks?: unknown[] } = {}) {
  const chats = new Map<string, Chat>();
  const repository: ChatRepository = {
    async list() { return structuredClone([...chats.values()]); },
    async get(id) { return structuredClone(chats.get(id) ?? null); },
    async create(title) {
      const chat = { id: `00000000-0000-4000-8000-${String(chats.size + 1).padStart(12, "0")}`,
        title, updatedAt: "2026-10-06T00:00:00.000Z" };
      chats.set(chat.id, structuredClone(chat));
      return structuredClone(chat);
    },
  };
  const records = new Map<string, Map<string, unknown>>();
  const bucket = (id: string): Map<string, unknown> => {
    let map = records.get(id);
    if (!map) { map = new Map(); records.set(id, map); }
    return map;
  };
  const peek = (id: string): Map<string, unknown> => records.get(id) ?? new Map<string, unknown>();
  const work: Promise<unknown>[] = [];
  const transports = [new FixtureTransport(plan)];
  const cores = new Map<string, ChatSessionCore>();
  const env: Env = { APP_AUTH_TOKEN: chatToken, CHAT_SESSIONS: {
    idFromName(name) { return name; },
    get(identity) {
      const id = String(identity);
      const state: SessionState = {
        storage: {
          async get<Value>(key: string) { return structuredClone(peek(id).get(key)) as Value | undefined; },
          async put(entries: Record<string, unknown>) { for (const [key, value] of Object.entries(entries)) bucket(id).set(key, structuredClone(value)); },
          async delete(keys: string[]) { let removed = false; for (const key of keys) removed = peek(id).delete(key) || removed; return removed; },
          async list(options: { prefix?: string } = {}) {
            const result = new Map<string, unknown>();
            for (const [key, value] of [...peek(id)].sort(([left], [right]) => left.localeCompare(right))) {
              if (options.prefix && !key.startsWith(options.prefix)) continue;
              result.set(key, structuredClone(value));
            }
            return result;
          },
        },
        waitUntil(promise) { work.push(promise); },
      };
      let core = cores.get(id);
      if (!core) {
        core = new ChatSessionCore(state, env, async () => transports[0] ?? new FixtureTransport(plan), repository);
        cores.set(id, core);
      }
      return { fetch: (request: Request) => core!.fetch(request) };
    },
  } };
  const request = (path: string, method = "GET", body?: unknown) => fetchApi(new Request(`https://app.invalid${path}`, {
    method, headers: { Authorization: `Bearer ${chatToken}`, ...(body === undefined ? {} : { "Content-Type": "application/json" }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  }), env, repository);
  const create = async (title = "Fixture chat"): Promise<Chat> => {
    const response = await request("/api/v2/chats", "POST", { title });
    expect(response.status).toBe(201);
    return await response.json() as Chat;
  };
  return { records, transports, request, create, drain: () => Promise.all(work) };
}

describe("reverse interactions round trip (P4)", () => {
  const permissionParams = (sessionId: string, toolCallId = "call-1") => ({
    sessionId,
    toolCall: { toolCallId, title: "Write file", rawInput: { path: "/workspace/secret.txt", token: "PRIVATE_ANSWER" } },
    options: [{ optionId: "allow_once", name: "Allow once" }, { optionId: "reject_once", name: "Reject" }],
  });
  const elicitationParams = (sessionId: string) => ({
    sessionId, mode: "form", message: "需要你的输入",
    requestedSchema: {
      properties: {
        name: { type: "string", title: "Name" },
        level: { type: "string", enum: ["a", "b"], title: "Level" },
      },
      required: ["name"],
    },
  });
  const answer = (app: Awaited<ReturnType<typeof fixture>>, chatId: string, interactionId: string, body: unknown) =>
    app.request(`/api/v2/chats/${chatId}/interactions/${encodeURIComponent(interactionId)}`, "POST", body);
  const detail = async (app: Awaited<ReturnType<typeof fixture>>, chatId: string) =>
    await (await app.request(`/api/v2/chats/${chatId}`)).json() as {
      entries: unknown[]; pendingInteractions: Array<{ id: string; status: string; fields?: unknown[] }>;
      interactions: Array<{ id: string; status: string; outcome?: string }>;
    };

  async function startRun(app: Awaited<ReturnType<typeof fixture>>, chat: Chat): Promise<void> {
    expect((await app.request(`/api/v2/chats/${chat.id}/messages`, "POST", { content: "Hello" })).status).toBe(202);
    await app.transports[0].promptStarted;
  }

  test("permission round trip: pending → browser answer → ACP response", async () => {
    const app = await fixture({ blocked: true });
    const chat = await app.create();
    await startRun(app, chat);
    const pending = app.transports[0].handler!("session/request_permission", permissionParams(chat.id));
    await until2(async () => (await detail(app, chat.id)).pendingInteractions.length === 1);
    const view = await detail(app, chat.id);
    const id = view.pendingInteractions[0]!.id;
    expect(id.startsWith("interaction:")).toBe(true);
    // 关联身份出现在读面；原始输入与答案不进 Yjs（记录只在生命周期点提交，读面即当前展示权威）。
    const visible = JSON.stringify(view);
    expect(visible).toContain(id);
    expect(visible).not.toContain("/workspace/secret.txt");
    expect((await answer(app, chat.id, id, { kind: "permission", decision: "allow_once" })).status).toBe(202);
    await expect(pending).resolves.toEqual({ outcome: { outcome: "selected", optionId: "allow_once" } });
    await until2(async () => (await detail(app, chat.id)).pendingInteractions.length === 0);
    const settled = await detail(app, chat.id);
    expect(settled.interactions.at(-1)).toMatchObject({ id, status: "resolved", outcome: "allow_once" });
  });

  test("elicitation round trip validates the form window before accepting", async () => {
    const app = await fixture({ blocked: true });
    const chat = await app.create();
    await startRun(app, chat);
    const first = app.transports[0].handler!("elicitation/create", elicitationParams(chat.id));
    await until2(async () => (await detail(app, chat.id)).pendingInteractions.length === 1);
    const view = await detail(app, chat.id);
    expect(view.pendingInteractions[0]!.fields).toEqual([
      { key: "name", title: "Name", description: "", type: "string", required: true },
      { key: "level", title: "Level", description: "", type: "string", required: false, enum: ["a", "b"] },
    ]);
    const id = view.pendingInteractions[0]!.id;
    expect((await answer(app, chat.id, id, { kind: "elicitation", decision: { action: "accept", content: { name: "Ada", level: "a" } } })).status).toBe(202);
    await expect(first).resolves.toEqual({ action: "accept", content: { name: "Ada", level: "a" } });
    expect(JSON.stringify(await detail(app, chat.id))).not.toContain("Ada");

    // 缺必填 / enum 越界的内容不误发 accept。
    const second = app.transports[0].handler!("elicitation/create", elicitationParams(chat.id));
    await until2(async () => (await detail(app, chat.id)).pendingInteractions.length === 1);
    const secondId = (await detail(app, chat.id)).pendingInteractions[0]!.id;
    expect((await answer(app, chat.id, secondId, { kind: "elicitation", decision: { action: "accept", content: { level: "z" } } })).status).toBe(202);
    await expect(second).resolves.toEqual({ action: "decline" });
  });

  test("unsupported forms decline without a callback and cannot be answered", async () => {
    const app = await fixture({ blocked: true });
    const chat = await app.create();
    await startRun(app, chat);
    const nested = app.transports[0].handler!("elicitation/create", {
      sessionId: chat.id, message: "嵌套表单",
      requestedSchema: { properties: { profile: { type: "object", title: "Profile" } } },
    });
    // 不支持结构默认拒绝：不会出现 pending，也不需要浏览器回答。
    await expect(nested).resolves.toEqual({ action: "decline" });
    await until2(async () => (await detail(app, chat.id)).interactions.length > 0);
    const view = await detail(app, chat.id);
    expect(view.pendingInteractions).toEqual([]);
    expect(view.interactions.at(-1)).toMatchObject({ status: "resolved", outcome: "decline" });
    expect((await answer(app, chat.id, view.interactions.at(-1)!.id, { kind: "elicitation", decision: { action: "accept", content: {} } })).status).toBe(409);
  });

  test("duplicate, unknown and cross-session answers are rejected", async () => {
    const app = await fixture({ blocked: true });
    const chat = await app.create();
    const other = await app.create("Another");
    await startRun(app, chat);
    const pending = app.transports[0].handler!("session/request_permission", permissionParams(chat.id));
    await until2(async () => (await detail(app, chat.id)).pendingInteractions.length === 1);
    const id = (await detail(app, chat.id)).pendingInteractions[0]!.id;
    expect((await answer(app, chat.id, id, { kind: "permission", decision: "reject_once" })).status).toBe(202);
    await expect(pending).resolves.toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
    // 重复回答、未知引用、跨会话与 kind 不符都被拒绝。
    expect((await answer(app, chat.id, id, { kind: "permission", decision: "allow_once" })).status).toBe(409);
    expect((await answer(app, chat.id, "interaction:missing", { kind: "permission", decision: "allow_once" })).status).toBe(409);
    expect((await answer(app, other.id, id, { kind: "permission", decision: "allow_once" })).status).toBe(409);
    const third = app.transports[0].handler!("session/request_permission", permissionParams(chat.id, "call-2"));
    await until2(async () => (await detail(app, chat.id)).pendingInteractions.length === 1);
    const thirdId = (await detail(app, chat.id)).pendingInteractions[0]!.id;
    expect((await answer(app, chat.id, thirdId, { kind: "elicitation", decision: { action: "decline" } })).status).toBe(400);
    expect((await answer(app, chat.id, thirdId, { kind: "permission", decision: "nonsense" })).status).toBe(400);
    expect((await answer(app, chat.id, thirdId, { kind: "permission", decision: "reject_once" })).status).toBe(202);
    await expect(third).resolves.toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
  });

  test("turn exit expires pending interactions and revokes the responder", async () => {
    const app = await fixture({ blocked: true });
    const chat = await app.create();
    await startRun(app, chat);
    const pending = app.transports[0].handler!("session/request_permission", permissionParams(chat.id));
    await until2(async () => (await detail(app, chat.id)).pendingInteractions.length === 1);
    const id = (await detail(app, chat.id)).pendingInteractions[0]!.id;
    // 取消并等待宿主关闭：live 队列按安全默认结算，投影置 expired。
    expect((await app.request(`/api/v2/chats/${chat.id}/cancel`, "POST")).status).toBe(200);
    await app.drain();
    await expect(pending).resolves.toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
    const settled = await detail(app, chat.id);
    expect(settled.pendingInteractions).toEqual([]);
    expect(settled.interactions.at(-1)).toMatchObject({ id, status: "expired" });
    // 迟到的回答不能复活已失效交互。
    expect((await answer(app, chat.id, id, { kind: "permission", decision: "allow_once" })).status).toBe(409);
  });

  test("pending interactions do not survive a restart and the answer path stays closed", async () => {
    const app = await fixture();
    const chat = await app.create();
    await storeFor(app.records, chat).commit({
      chat, running: false, executionBlocked: false,
      checkpoint: {
        checkpointVersion: 1, schemaVersions: versionedPair, session: {}, entries: [], toolCalls: [], turns: [],
        tasks: [], plansByTurn: {},
        interactions: [{ id: "interaction:stale", kind: "permission", sessionId: chat.id, status: "pending",
          createdAt: "2026-10-09T00:00:00Z", resolvedAt: null, title: "Write file",
          options: [{ optionId: "allow_once", name: "Allow once" }] }],
        payloadValues: [],
      },
    });
    const view = await detail(app, chat.id);
    expect(view.pendingInteractions).toEqual([]);
    expect(view.interactions.at(-1)).toMatchObject({ id: "interaction:stale", status: "expired" });
    expect((await answer(app, chat.id, "interaction:stale", { kind: "permission", decision: "allow_once" })).status).toBe(409);
  });

  test("declares the task snapshot capability and seeds load configuration", async () => {
    const app = await fixture({ blocked: true, loadConfig: [{ id: "model", name: "Model", type: "select",
      currentValue: "sonnet", options: [{ value: "sonnet", name: "Sonnet" }] }],
      tasks: [{ task_id: "task-1", kind: "shell", summary: "Checking", status: "completed", started_at: "2026-10-10T00:00:00Z" }] });
    const chat = await app.create();
    await startRun(app, chat);
    const initialize = app.transports[0].calls.find(({ method }) => method === "initialize");
    expect((initialize?.params as { clientCapabilities?: { _meta?: Record<string, unknown> } }).clientCapabilities?._meta)
      .toMatchObject({ "peri.userInputQueue": true, "peri.unstableEvent": true });
    // 配置源进入会话读面。
    await until2(async () => {
      const state = await detail(app, chat.id) as unknown as { session?: { configOptions?: unknown[] } };
      return (state.session?.configOptions?.length ?? 0) > 0;
    });
    // 后台任务修订缺口触发 session/bg-tasks 快照修复（沿用现有 SDK 投影规则）。
    const transport = app.transports[0];
    transport.emitRaw({ jsonrpc: "2.0", method: "peri/unstable_event", params: { sessionId: chat.id,
      event: "bg-task-updated", data: { task_id: "task-1", revision: 1, status: "running" } } });
    transport.emitRaw({ jsonrpc: "2.0", method: "peri/unstable_event", params: { sessionId: chat.id,
      event: "bg-task-updated", data: { task_id: "task-1", revision: 5, status: "completed" } } });
    await until2(async () => transport.calls.some(({ method }) => method === "session/bg-tasks"));
    await until2(async () => {
      const state = await detail(app, chat.id) as unknown as { tasks?: Array<{ taskId: string; status: string }> };
      return state.tasks?.some((task) => task.taskId === "task-1") === true;
    });
  });
});
