import { describe, expect, test } from "bun:test";
import { createPeriSession } from "../worker/chat/creation";
import type { AcpTransport, Env } from "../worker/types";

const sessionId = "00000000-0000-4000-8000-000000000001";

function fixture(options: { replies?: Record<string, unknown>; failMethod?: string; closeFailure?: boolean } = {}) {
  const calls: { method: string; params?: unknown }[] = [];
  let closeCalls = 0;
  let handler: ((method: string, params: unknown) => unknown | Promise<unknown>) | undefined;
  const transport: AcpTransport = {
    async request<Result>(method: string, params?: unknown): Promise<Result> {
      calls.push({ method, params: structuredClone(params) });
      if (method === options.failMethod) throw new Error(`${method} failed`);
      if (options.replies && Object.hasOwn(options.replies, method)) return options.replies[method] as Result;
      if (method === "initialize") return { protocolVersion: 1 } as Result;
      if (method === "session/new") return { sessionId } as Result;
      if (method === "session/rename") return { sessionId, title: (params as { title: string }).title } as Result;
      throw new Error(`Unexpected creation request: ${method}`);
    },
    async sendRequest() { throw new Error("Creation must not send a prompt"); },
    async notify() { throw new Error("Creation must not send a notification"); },
    async *events() {},
    subscribe() { throw new Error("Creation must not subscribe to execution"); },
    setRequestHandler(callback) { handler = callback; },
    async close() { closeCalls++; if (options.closeFailure) throw new Error("close unconfirmed"); },
  };
  const env: Env = {
    CHAT_SESSIONS: {
      idFromName() { throw new Error("Creation must not touch a chat DO"); },
      get() { throw new Error("Creation must not touch a chat DO"); },
    },
  };
  const startTransport = async (received: Env) => { expect(received).toBe(env); return transport; };
  const pending: Promise<unknown>[] = [];
  const create = () => createPeriSession(env, "Requested title", startTransport, (work) => { pending.push(work); });
  return { calls, transport, env, startTransport, create, pending, closeCalls: () => closeCalls, handler: () => handler! };
}

describe("ACP session creation without chat DO or execution dispatch", () => {
  test("initializes, creates a real UUID, renames and closes before returning its identity", async () => {
    const app = fixture();
    expect(await app.create()).toBe(sessionId);
    expect(app.calls.map(({ method }) => method)).toEqual(["initialize", "session/new", "session/rename"]);
    expect(app.calls[0].params).toMatchObject({ protocolVersion: 1, clientCapabilities: { _meta: { "peri.userInputQueue": true } } });
    expect((app.calls[0].params as { clientCapabilities: { _meta: Record<string, unknown> } }).clientCapabilities._meta)
      .not.toHaveProperty("peri.executionProtocol");
    expect(app.calls[1].params).toEqual({ cwd: "/workspace", mcpServers: [] });
    expect(app.calls[2].params).toEqual({ sessionId, title: "Requested title" });
    expect(app.closeCalls()).toBe(1);
    expect(app.pending).toEqual([]);
  });

  test.each(["initialize", "session/new", "session/rename"])("closes the Host if %s rejects", async (method) => {
    const app = fixture({ failMethod: method });
    await expect(app.create()).rejects.toThrow(`${method} failed`);
    expect(app.closeCalls()).toBe(1);
  });

  test.each([{ protocolVersion: 2 }, {}, null])("rejects invalid initialization response %j and closes", async (reply) => {
    const app = fixture({ replies: { initialize: reply } });
    await expect(app.create()).rejects.toThrow("Unsupported ACP protocol version");
    expect(app.calls.map(({ method }) => method)).toEqual(["initialize"]);
    expect(app.closeCalls()).toBe(1);
  });

  test.each([{ sessionId: "presentation-id" }, {}, { sessionId: 42 }])("rejects invalid actual session identity %j and closes", async (reply) => {
    const app = fixture({ replies: { "session/new": reply } });
    await expect(app.create()).rejects.toThrow("Invalid ACP session/new identity");
    expect(app.calls.some(({ method }) => method === "session/rename")).toBe(false);
    expect(app.closeCalls()).toBe(1);
  });

  test.each([{ sessionId, title: "Wrong title" }, { sessionId: "wrong-id", title: "Requested title" }, null])(
    "rejects invalid rename receipt %j and closes", async (reply) => {
      const app = fixture({ replies: { "session/rename": reply } });
      await expect(app.create()).rejects.toThrow("Invalid ACP session/rename response");
      expect(app.closeCalls()).toBe(1);
    },
  );

  test("an unconfirmed close is not reported as successful creation", async () => {
    const app = fixture({ closeFailure: true });
    await expect(app.create()).rejects.toThrow("close unconfirmed");
    expect(app.closeCalls()).toBe(1);
  });

  test("denies reverse permissions and unsupported capabilities", async () => {
    const app = fixture();
    await app.create();
    expect(await app.handler()("session/request_permission", {})).toEqual({ outcome: { outcome: "cancelled" } });
    expect(() => app.handler()("fs/read_text_file", {})).toThrow("not authorized");
  });

  test("startup rejection propagates without inventing a transport to close", async () => {
    const app = fixture();
    await expect(createPeriSession(app.env, "New", async () => { throw new Error("startup failed"); }, () => {})).rejects.toThrow("startup failed");
    expect(app.closeCalls()).toBe(0);
    expect(app.calls).toEqual([]);
  });
});
