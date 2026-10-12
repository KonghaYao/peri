import { afterEach, describe, expect, test } from "bun:test";
import { Hono } from "hono";
import { HTTPException } from "hono/http-exception";
import { createApi } from "../worker/api/router";
import { relayRequest } from "../worker/api/relay";

/**
 * relay 的 MCP 通道（`/api/relay/mcp/...`）：上游目标由路径第一段携带，
 * 请求与响应（含 SSE 流、`mcp-session-id`）原样透传，不做存储的 `base_url` 改写。
 */

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const ORIGIN = "https://peri.example.com";
const UPSTREAM = "https://mcp.example.com/base";
const originalFetch = globalThis.fetch;

afterEach(() => {
  globalThis.fetch = originalFetch;
});

interface Forwarded {
  url: string;
  method: string;
  headers: Headers;
  body: string;
}

async function readBody(body: RequestInit["body"]): Promise<string> {
  if (!body) return "";
  if (typeof body === "string") return body;
  if (body instanceof ReadableStream || body instanceof ArrayBuffer || ArrayBuffer.isView(body)) {
    return await new Response(body as BodyInit).text();
  }
  return "";
}

/** 替身上游：记录转发形态，响应由用例给出。 */
function stubUpstream(respond: () => Response): Forwarded[] {
  const forwarded: Forwarded[] = [];
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    forwarded.push({
      url: String(input),
      method: (init?.method ?? "GET").toUpperCase(),
      headers: new Headers(init?.headers),
      body: await readBody(init?.body),
    });
    return respond();
  }) as typeof fetch;
  return forwarded;
}

function mcpRequest(path = "/messages?session=abc", init?: RequestInit): Request {
  return new Request(`${ORIGIN}/api/relay/mcp/${encodeURIComponent(UPSTREAM)}${path}`, init);
}

describe("mcp relay 转发形态", () => {
  test("保留上游路径、查询、方法与请求体，客户端头原样上行、逐跳头剥离", async () => {
    const forwarded = stubUpstream(() =>
      new Response("{}", { status: 200, headers: { "content-type": "application/json" } }));
    const response = await relayRequest(
      mcpRequest("/messages?session=abc", {
        method: "POST",
        headers: {
          authorization: "Bearer mcp-token",
          "mcp-protocol-version": "2026-07-28",
          "x-tenant": "acme",
          origin: ORIGIN,
        },
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }),
      }),
      "mcp",
    );

    expect(response.status).toBe(200);
    expect(forwarded).toHaveLength(1);
    expect(forwarded[0].url).toBe("https://mcp.example.com/base/messages?session=abc");
    expect(forwarded[0].method).toBe("POST");
    expect(forwarded[0].headers.get("authorization")).toBe("Bearer mcp-token");
    expect(forwarded[0].headers.get("mcp-protocol-version")).toBe("2026-07-28");
    expect(forwarded[0].headers.get("x-tenant")).toBe("acme");
    expect(forwarded[0].headers.get("origin")).toBeNull();
    expect(JSON.parse(forwarded[0].body)).toEqual({ jsonrpc: "2.0", id: 1, method: "tools/list" });
  });

  test("会话头与响应体原样回传，跳源头被剥离", async () => {
    stubUpstream(() =>
      new Response("event: message\ndata: {}\n\n", {
        status: 200,
        headers: {
          "content-type": "text/event-stream",
          "mcp-session-id": "session-1",
          "cf-ray": "abcd-SJC",
        },
      }));
    const response = await relayRequest(mcpRequest(), "mcp");

    expect(response.headers.get("content-type")).toBe("text/event-stream");
    expect(response.headers.get("mcp-session-id")).toBe("session-1");
    expect(response.headers.get("cf-ray")).toBeNull();
    expect(await response.text()).toBe("event: message\ndata: {}\n\n");
  });

  test("SSE 流在上游未结束时即可返回，不被缓冲", async () => {
    let upstream!: ReadableStreamDefaultController<Uint8Array>;
    stubUpstream(() =>
      new Response(
        new ReadableStream<Uint8Array>({
          start(controller) {
            upstream = controller;
            controller.enqueue(encoder.encode('event: message\ndata: {"ok":true}\n\n'));
          },
        }),
        { status: 200, headers: { "content-type": "text/event-stream" } },
      ));
    // 上游流保持打开：若 relay 缓冲整段正文，这里会一直挂住。
    const response = await relayRequest(mcpRequest(), "mcp");
    const reader = response.body!.getReader();
    expect(decoder.decode((await reader.read()).value)).toContain("event: message");
    upstream.close();
    await reader.cancel();
  });

  test("mcp 正文不做存储的 base_url 改写", async () => {
    const payload = JSON.stringify({
      result: { note: "base_url", nested: { base_url: "https://mcp.example.com/keep" } },
    });
    stubUpstream(() =>
      new Response(payload, { status: 200, headers: { "content-type": "application/json" } }));
    const response = await relayRequest(mcpRequest(), "mcp");
    expect(await response.text()).toBe(payload);
  });
});

describe("mcp relay 目标校验与路由", () => {
  test("缺少或非 http(s) 的基址以 400 拒绝，且不发起上游请求", async () => {
    const forwarded = stubUpstream(() => new Response("{}"));
    for (const path of [
      "/api/relay/mcp",
      `/api/relay/mcp/${encodeURIComponent("ftp://mcp.example.com")}`,
      "/api/relay/mcp/%E4%B8%8D%E6%98%AFURL/messages",
    ]) {
      const failure = await relayRequest(new Request(`${ORIGIN}${path}`, { method: "POST" }), "mcp")
        .catch((error: unknown) => error);
      expect(failure).toBeInstanceOf(HTTPException);
      expect((failure as HTTPException).status).toBe(400);
    }
    expect(forwarded).toHaveLength(0);
  });

  test("路由表把 /api/relay/mcp/* 接到 mcp 转发", async () => {
    const forwarded = stubUpstream(() =>
      new Response('{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2026-07-28"}}', {
        status: 200,
        headers: { "content-type": "application/json" },
      }));
    const response = await createApi().request(
      `${ORIGIN}/api/relay/mcp/${encodeURIComponent(UPSTREAM)}/discover`,
      { method: "POST", body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "server/discover" }) },
    );

    expect(response.status).toBe(200);
    expect(forwarded[0].url).toBe("https://mcp.example.com/base/discover");
    expect(await response.json()).toEqual({
      jsonrpc: "2.0",
      id: 1,
      result: { protocolVersion: "2026-07-28" },
    });
  });
});
