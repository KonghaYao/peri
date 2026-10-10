import { ChatSessionCore } from "../worker/chat/session";
import type { HibernatingSessionState } from "../worker/chat/sync";
import type { AcpTransport, Env } from "../worker/types";
import type { JsonRpcNotification } from "../worker/sdk";

const chat = { id: "00000000-0000-4000-8000-000000000001", title: "Workerd WS fixture", updatedAt: "2026-10-07T00:00:00Z" };

export class FixtureTransport implements AcpTransport {
  readonly generationId = crypto.randomUUID();
  readonly listeners = new Set<(notification: JsonRpcNotification) => void>();
  cancels = 0;
  closeFailure = false;
  handler?: (method: string, params: unknown) => unknown | Promise<unknown>;
  /** 每轮产出与宿主行为开关（供 P5 载荷/交互/断宿主验收）。 */
  plan: { toolResult?: string; blocked?: boolean } = {};
  private finish!: (value: { stopReason: string }) => void;
  private readonly response = new Promise<{ stopReason: string }>((resolve) => { this.finish = resolve; });
  private stopped = false;

  async request<Result>(method: string): Promise<Result> {
    return (method === "initialize" ? { protocolVersion: 1 } : {}) as Result;
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
  private async produce(): Promise<void> {
    for (let chunk = 0; chunk < 35 && !this.stopped; chunk++) {
      await new Promise((resolve) => setTimeout(resolve, 25));
      if (this.stopped) return;
      for (const listener of this.listeners) listener({ jsonrpc: "2.0", method: "session/update", params: {
        sessionId: chat.id, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "piece " } },
      } });
    }
    if (this.plan.toolResult !== undefined && !this.stopped) {
      for (const listener of this.listeners) listener({ jsonrpc: "2.0", method: "session/update", params: {
        sessionId: chat.id, update: { sessionUpdate: "tool_call", toolCallId: "tool-1", title: "Read", status: "completed",
          rawOutput: this.plan.toolResult, content: [{ type: "text", text: "big result" }] },
      } });
    }
    if (!this.stopped && !this.plan.blocked) this.finish({ stopReason: "end_turn" });
  }
}

export class TestSession extends ChatSessionCore {
  private readonly identity = crypto.randomUUID();
  private readonly transports: FixtureTransport[];
  /** 应用到下一次运行的夹具计划（每个 run 都会新建 transport）。 */
  private nextPlan: { toolResult?: string; blocked?: boolean; closeFailure?: boolean } = {};

  constructor(private readonly context: HibernatingSessionState, env: Env) {
    const transports: FixtureTransport[] = [];
    const planFor = (): { toolResult?: string; blocked?: boolean; closeFailure?: boolean } => ({ ...this.nextPlan });
    super(context, env, async () => {
      const starts = await context.storage.get<number>("host-starts") ?? 0;
      await context.storage.put({ "host-starts": starts + 1 });
      const transport = new FixtureTransport();
      const plan = planFor();
      transport.plan = { ...transport.plan, ...plan };
      transport.closeFailure = plan.closeFailure === true;
      transports.push(transport);
      return transport;
    }, { async get(id) { return id === chat.id ? chat : null; } });
    this.transports = transports;
  }

  /** 同时作用于当前运行与下一次运行（路线在 run 前后调用都成立）。 */
  private applyPlan(patch: { toolResult?: string; blocked?: boolean; closeFailure?: boolean }): void {
    this.nextPlan = { ...this.nextPlan, ...patch };
    const current = this.transports.at(-1);
    if (!current) return;
    current.plan = { ...current.plan, ...(patch.toolResult !== undefined ? { toolResult: patch.toolResult } : {}),
      ...(patch.blocked !== undefined ? { blocked: patch.blocked } : {}) };
    if (patch.closeFailure !== undefined) current.closeFailure = patch.closeFailure;
  }

  async fetch(request: Request): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/fixture/tool-result") {
      const body = await request.json() as { text?: string };
      this.applyPlan({ toolResult: body.text ?? "x".repeat(6000) });
      return Response.json({ ok: true });
    }
    if (path === "/fixture/blocked") {
      this.applyPlan({ blocked: true });
      return Response.json({ ok: true });
    }
    if (path === "/fixture/close-failure") {
      this.applyPlan({ closeFailure: true });
      return Response.json({ ok: true });
    }
    if (path === "/fixture/permission") {
      const transport = this.transports.at(-1);
      const handler = transport?.handler;
      if (!handler) return new Response(null, { status: 409 });
      const body = await request.json() as { toolCallId?: string; elicitation?: boolean };
      const params = body.elicitation
        ? { sessionId: chat.id, message: "需要你的输入",
            requestedSchema: { properties: { name: { type: "string", title: "Name" } }, required: ["name"] } }
        : { sessionId: chat.id,
            toolCall: { toolCallId: body.toolCallId ?? "tool-1", title: "Write", rawInput: { path: "/workspace/secret.txt" } },
            options: [{ optionId: "allow_once", name: "Allow" }, { optionId: "reject_once", name: "Reject" }] };
      const pending = handler(body.elicitation ? "elicitation/create" : "session/request_permission", params);
      return Response.json({ result: await pending });
    }
    if (new URL(request.url).pathname === "/stats") {
      if (request.headers.get("Authorization") !== "Bearer workerd-test-token") return new Response(null, { status: 401 });
      const sockets = this.context.getWebSockets!();
      return Response.json({ identity: this.identity, sockets: sockets.length,
        bufferedAmountType: sockets.length ? typeof Reflect.get(sockets[0]!, "bufferedAmount") : null,
        attachments: sockets.map((socket) => socket.deserializeAttachment!()),
        starts: await this.context.storage.get<number>("host-starts") ?? 0,
        cancels: this.transports.reduce((total, transport) => total + transport.cancels, 0),
      });
    }
    return super.fetch(request);
  }
}

export default {
  async fetch(request: Request, env: { CHAT_SESSIONS: Env["CHAT_SESSIONS"] }): Promise<Response> {
    return env.CHAT_SESSIONS.get(env.CHAT_SESSIONS.idFromName("chat")).fetch(request);
  },
};
