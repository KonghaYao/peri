import { ChatSessionCore } from "../worker/chat/session";
import type { HibernatingSessionState } from "../worker/chat/sync";
import type { AcpTransport, Env } from "../worker/types";
import type { JsonRpcNotification } from "../worker/sdk";

const chat = { id: "00000000-0000-4000-8000-000000000001", title: "Workerd WS fixture", updatedAt: "2026-10-07T00:00:00Z" };

class FixtureTransport implements AcpTransport {
  readonly generationId = crypto.randomUUID();
  readonly listeners = new Set<(notification: JsonRpcNotification) => void>();
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
  async notify(): Promise<void> {}
  async *events(): AsyncIterable<JsonRpcNotification> {}
  subscribe(listener: (notification: JsonRpcNotification) => void) {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
  setRequestHandler(): void {}
  async close(): Promise<void> { this.stopped = true; }
  stop(): void { this.stopped = true; this.finish({ stopReason: "cancelled" }); }
  private async produce(): Promise<void> {
    for (let chunk = 0; chunk < 35 && !this.stopped; chunk++) {
      await new Promise((resolve) => setTimeout(resolve, 25));
      if (this.stopped) return;
      for (const listener of this.listeners) listener({ jsonrpc: "2.0", method: "session/update", params: {
        sessionId: chat.id, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "piece " } },
      } });
    }
    if (!this.stopped) this.finish({ stopReason: "end_turn" });
  }
}

export class TestSession extends ChatSessionCore {
  private readonly identity = crypto.randomUUID();

  constructor(private readonly context: HibernatingSessionState, env: Env) {
    let transport: FixtureTransport;
    super(context, env, async () => {
      const starts = await context.storage.get<number>("host-starts") ?? 0;
      await context.storage.put("host-starts", starts + 1);
      transport = new FixtureTransport();
      return transport;
    }, () => ({
      handle() { return undefined; }, seal() {},
      async stop() {
        transport.stop();
        return { lifecycle: 1, controlGeneration: 1, resumeCommandId: "own-stop" };
      },
      async resume() {
        const resumes = await context.storage.get<number>("resumes") ?? 0;
        await context.storage.put("resumes", resumes + 1);
      },
      async stopAfterHostClose() {},
    }), { async get(id) { return id === chat.id ? chat : null; } });
  }

  async fetch(request: Request): Promise<Response> {
    if (new URL(request.url).pathname === "/stats") {
      if (request.headers.get("Authorization") !== "Bearer workerd-test-token") return new Response(null, { status: 401 });
      const sockets = this.context.getWebSockets!();
      return Response.json({ identity: this.identity, sockets: sockets.length,
        bufferedAmountType: sockets.length ? typeof Reflect.get(sockets[0]!, "bufferedAmount") : null,
        attachments: sockets.map((socket) => socket.deserializeAttachment!()),
        starts: await this.context.storage.get<number>("host-starts") ?? 0,
        resumes: await this.context.storage.get<number>("resumes") ?? 0,
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
