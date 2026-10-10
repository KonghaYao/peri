import { createApi } from "../worker/api/router";
import { ChatRegistry } from "../worker/chat/registry";
import type { AcpTransport, Chat, ChatRepository, Env, StartTransport } from "../worker/types";
import type { JsonRpcNotification } from "../worker/sdk";

/**
 * 无 DO 的 workerd 夹具：会话运行期只在 isolate 内存，持久历史由绑定 `REPLAY_HISTORY` 模拟 Turso，
 * 因此 `setOptions`（isolate 重启）后仍能按真实 `session/load` 重放复原。
 */

const chat: Chat = { id: "00000000-0000-4000-8000-000000000001", title: "Workerd fixture", updatedAt: "2026-10-07T00:00:00Z" };

type FixtureEnv = Env & { REPLAY_HISTORY?: string };

class FixtureTransport implements AcpTransport {
  readonly generationId = crypto.randomUUID();
  readonly listeners = new Set<(notification: JsonRpcNotification) => void>();
  readonly notifications: JsonRpcNotification[];
  cancels = 0;
  closeFailure = false;
  blocked = false;
  handler?: (method: string, params: unknown) => unknown | Promise<unknown>;
  private finish!: (value: { stopReason: string }) => void;
  private readonly response = new Promise<{ stopReason: string }>((resolve) => { this.finish = resolve; });
  private stopped = false;
  private sessionId = "";

  constructor(notifications: JsonRpcNotification[], plan: { toolResult?: string;
    permission?: { kind: "permission" | "elicitation"; toolCallId: string } }) {
    this.notifications = notifications;
    this.toolResult = plan.toolResult;
    this.permission = plan.permission;
  }

  private readonly toolResult?: string;
  private readonly permission?: { kind: "permission" | "elicitation"; toolCallId: string };

  async request<Result>(method: string, params?: unknown): Promise<Result> {
    if (method === "initialize") return { protocolVersion: 1 } as Result;
    if (method === "session/load") {
      this.sessionId = (params as { sessionId: string }).sessionId;
      // ACP 语义：响应之前重放历史。
      for (const notification of this.notifications) this.emitRaw(notification);
      return { modes: {} } as Result;
    }
    if (method === "session/bg-tasks") return { revision: 0, tasks: [] } as Result;
    throw new Error(`Unexpected ACP request: ${method}`);
  }

  async sendRequest<Result>() {
    void this.produce();
    return { response: this.response as Promise<Result> };
  }

  async notify(method: string): Promise<void> {
    if (method !== "session/cancel") throw new Error(`Unexpected ACP notification: ${method}`);
    this.cancels++;
    this.stop();
  }

  async *events(): AsyncIterable<JsonRpcNotification> {}

  subscribe(listener: (notification: JsonRpcNotification) => void) {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  setRequestHandler(handler: (method: string, params: unknown) => unknown | Promise<unknown>): void {
    this.handler = handler;
  }

  async close(): Promise<void> {
    if (this.closeFailure) throw new Error("workerd fixture close failure");
    this.stopped = true;
  }

  stop(): void { this.stopped = true; this.finish({ stopReason: "cancelled" }); }

  emitRaw(notification: JsonRpcNotification): void {
    for (const listener of this.listeners) listener(notification);
  }

  private async produce(): Promise<void> {
    for (let chunk = 0; chunk < 35 && !this.stopped; chunk++) {
      await new Promise((resolve) => setTimeout(resolve, 25));
      if (this.stopped) return;
      this.emitRaw({ jsonrpc: "2.0", method: "session/update", params: {
        sessionId: chat.id, update: { sessionUpdate: "agent_message_chunk", messageId: "live-answer",
          content: { type: "text", text: "piece " } },
      } });
    }
    if (this.permission && !this.stopped && this.handler) {
      // 反向请求由宿主在轮次内发起：resolver 与被解决的 promise 同属一个请求上下文。
      const params = this.permission.kind === "elicitation"
        ? { sessionId: chat.id, message: "需要你的输入",
            requestedSchema: { properties: { name: { type: "string", title: "Name" } }, required: ["name"] } }
        : { sessionId: chat.id,
            toolCall: { toolCallId: this.permission.toolCallId, title: "Write",
              rawInput: { path: "/workspace/secret.txt" } },
            options: [{ optionId: "allow_once", name: "Allow" }, { optionId: "reject_once", name: "Reject" }] };
      const outcome = await this.handler(this.permission.kind === "elicitation"
        ? "elicitation/create" : "session/request_permission", params) as Record<string, unknown>;
      state.permissionOutcome = JSON.stringify(outcome);
    }
    if (this.toolResult !== undefined && !this.stopped) {
      this.emitRaw({ jsonrpc: "2.0", method: "session/update", params: {
        sessionId: chat.id, update: { sessionUpdate: "tool_call", toolCallId: "tool-1", title: "Read", status: "completed",
          rawOutput: this.toolResult, content: [{ type: "text", text: "big result" }] },
      } });
    }
    if (!this.stopped && !this.blocked) this.finish({ stopReason: "end_turn" });
  }
}

interface FixtureState {
  /** isolate 身份：只在 handler 内惰性生成（workerd 禁止在全局作用域取随机值）。 */
  identity(): string;
  starts: number;
  transports: FixtureTransport[];
  plan: { toolResult?: string; blocked?: boolean; closeFailure?: boolean;
    permission?: { kind: "permission" | "elicitation"; toolCallId: string } };
  /** 最近一次反向请求的结算结果（测试从 /stats 读，避免跨请求解决 promise）。 */
  permissionOutcome: string | null;
}

let isolateIdentity: string | undefined;
const state: FixtureState = {
  identity: () => (isolateIdentity ??= crypto.randomUUID()),
  starts: 0, transports: [], plan: {}, permissionOutcome: null,
};

const repository: ChatRepository = {
  async list() { return [chat]; },
  async get(id) { return id === chat.id ? chat : null; },
  async create() { return chat; },
};

const runtimes = new ChatRegistry();

const startTransport: StartTransport = async (env) => {
  state.starts++;
  const history = JSON.parse((env as FixtureEnv).REPLAY_HISTORY ?? "[]") as JsonRpcNotification[];
  const transport = new FixtureTransport(history, { toolResult: state.plan.toolResult, permission: state.plan.permission });
  transport.blocked = state.plan.blocked === true;
  transport.closeFailure = state.plan.closeFailure === true;
  state.transports.push(transport);
  return transport;
};

const api = createApi({ repository: () => repository, startTransport, runtimes });

export default {
  async fetch(request: Request, env: FixtureEnv,
    context: { waitUntil(promise: Promise<unknown>): void }): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/fixture/tool-result") {
      state.plan.toolResult = ((await request.json()) as { text?: string }).text ?? "x".repeat(6000);
      return Response.json({ ok: true });
    }
    if (path === "/fixture/blocked") { state.plan.blocked = true; return Response.json({ ok: true }); }
    if (path === "/fixture/close-failure") { state.plan.closeFailure = true; return Response.json({ ok: true }); }
    if (path === "/fixture/permission") {
      const body = await request.json() as { toolCallId?: string; elicitation?: boolean };
      state.permissionOutcome = null;
      state.plan.permission = { kind: body.elicitation ? "elicitation" : "permission",
        toolCallId: body.toolCallId ?? "tool-1" };
      return Response.json({ ok: true });
    }
    if (path === "/stats") {
      if (request.headers.get("Authorization") !== "Bearer workerd-test-token") return new Response(null, { status: 401 });
      const runtime = runtimes.peek(chat.id);
      return Response.json({
        identity: state.identity(),
        starts: state.starts,
        cancels: state.transports.reduce((total, transport) => total + transport.cancels, 0),
        running: runtime?.running ?? false,
        executionBlocked: runtime?.executionBlocked ?? false,
        resident: runtime !== undefined,
        permissionOutcome: state.permissionOutcome,
      });
    }
    return api.fetch(request, env, context as never);
  },
};
