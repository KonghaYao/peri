import { afterEach, describe, expect, test } from "bun:test";
import { ChatCommandError } from "@core/agent";
import { RemoteChatRuntime } from "./runtime";

const chatId = "11111111-1111-4111-8111-111111111111";
const originalFetch = globalThis.fetch;

afterEach(() => { globalThis.fetch = originalFetch; });

function runtime(): RemoteChatRuntime {
  return new RemoteChatRuntime(chatId, "https://example.test");
}

function mockFetch(respond: () => Response | Promise<Response>): void {
  globalThis.fetch = (async () => respond()) as unknown as typeof fetch;
}

async function commandFailure(): Promise<ChatCommandError> {
  try {
    await runtime().command({ op: "cancel" });
  } catch (error) {
    expect(error).toBeInstanceOf(ChatCommandError);
    return error as ChatCommandError;
  }
  throw new Error("expected the command to fail");
}

describe("RemoteChatRuntime command contract", () => {
  test("网络拒绝无法证明命令未生效，映射为 transport_error/unknown", async () => {
    mockFetch(() => { throw new TypeError("Failed to fetch"); });
    const failure = await commandFailure();
    expect(failure.code).toBe("transport_error");
    expect(failure.disposition).toBe("unknown");
    expect(failure.message).toContain("Failed to fetch");
  });

  test("非法 JSON 响应映射为 transport_error/unknown", async () => {
    mockFetch(() => new Response("not-json", { status: 200 }));
    const failure = await commandFailure();
    expect(failure.code).toBe("transport_error");
    expect(failure.disposition).toBe("unknown");
  });

  test("409 busy 保留服务端 rejected 处置", async () => {
    mockFetch(() => new Response(JSON.stringify({
      ok: false, code: "busy", message: "Chat is busy", disposition: "rejected",
    }), { status: 409 }));
    const failure = await commandFailure();
    expect(failure.code).toBe("busy");
    expect(failure.message).toBe("Chat is busy");
    expect(failure.disposition).toBe("rejected");
  });

  test("500 internal_error 保留 unknown 处置", async () => {
    mockFetch(() => new Response(JSON.stringify({
      ok: false, code: "internal_error", message: "命令执行失败；诊断已记录", disposition: "unknown",
    }), { status: 500 }));
    const failure = await commandFailure();
    expect(failure.code).toBe("internal_error");
    expect(failure.disposition).toBe("unknown");
  });

  test("404 not_found 保留 rejected 处置", async () => {
    mockFetch(() => new Response(JSON.stringify({
      ok: false, code: "not_found", message: "Chat not found", disposition: "rejected",
    }), { status: 404 }));
    const failure = await commandFailure();
    expect(failure.code).toBe("not_found");
    expect(failure.disposition).toBe("rejected");
  });

  test("非 2xx 却声称成功时按传输失败处理", async () => {
    mockFetch(() => new Response(JSON.stringify({ ok: true }), { status: 500 }));
    const failure = await commandFailure();
    expect(failure.code).toBe("transport_error");
    expect(failure.disposition).toBe("unknown");
  });

  test("成功响应返回载荷", async () => {
    mockFetch(() => new Response(JSON.stringify({
      ok: true, payload: { id: "ref-1", bytes: 2, json: "{}" },
    }), { status: 200 }));
    await expect(runtime().command({ op: "payload", refId: "ref-1" })).resolves.toEqual({
      id: "ref-1", bytes: 2, json: "{}",
    });
  });
});
