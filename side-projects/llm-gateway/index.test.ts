import { describe, test, expect, beforeEach, mock } from "bun:test";
import {
  loadConfig,
  resolveUrl,
  headersForLog,
  createHandler,
  proxyRequest,
  resetReqCounter,
  type GatewayConfig,
} from "./index";

// ---------- 测试用配置 ----------

function makeTestConfig(overrides?: Partial<GatewayConfig>): GatewayConfig {
  return {
    port: 3456,
    openaiBase: "https://api.openai.com",
    anthropicBase: "https://api.anthropic.com",
    logLevel: "none",
    logDir: "/tmp/llm-gateway-test",
    ...overrides,
  };
}

// ---------- loadConfig ----------

describe("loadConfig", () => {
  test("默认值", () => {
    const cfg = loadConfig({});
    expect(cfg.port).toBe(3456);
    expect(cfg.openaiBase).toBe("https://api.openai.com");
    expect(cfg.anthropicBase).toBe("https://api.anthropic.com");
    expect(cfg.logLevel).toBe("summary");
    expect(cfg.logDir).toBe("./data");
  });

  test("从环境变量读取", () => {
    const cfg = loadConfig({
      PORT: "9999",
      OPENAI_BASE_URL: "https://custom.openai.com/api/",
      ANTHROPIC_BASE_URL: "https://custom.anthropic.com/",
      LOG_LEVEL: "body",
      LOG_DIR: "/tmp/logs",
    });
    expect(cfg.port).toBe(9999);
    expect(cfg.openaiBase).toBe("https://custom.openai.com/api");
    expect(cfg.anthropicBase).toBe("https://custom.anthropic.com");
    expect(cfg.logLevel).toBe("body");
    expect(cfg.logDir).toBe("/tmp/logs");
  });

  test("尾部斜杠去除", () => {
    const cfg = loadConfig({
      OPENAI_BASE_URL: "https://api.openai.com///",
      ANTHROPIC_BASE_URL: "https://api.anthropic.com/",
    });
    expect(cfg.openaiBase).toBe("https://api.openai.com");
    expect(cfg.anthropicBase).toBe("https://api.anthropic.com");
  });
});

// ---------- resolveUrl ----------

describe("resolveUrl", () => {
  test("简单路径拼接", () => {
    const url = resolveUrl("https://api.openai.com", "/v1/chat/completions", "");
    expect(url).toBe("https://api.openai.com/v1/chat/completions");
  });

  test("带 query string", () => {
    const url = resolveUrl("https://api.openai.com", "/v1/models", "?type=chat");
    expect(url).toBe("https://api.openai.com/v1/models?type=chat");
  });

  test("base 带路径前缀且请求路径重叠时去重", () => {
    const url = resolveUrl("https://api.openai.com/v1", "/v1/chat/completions", "");
    expect(url).toBe("https://api.openai.com/v1/chat/completions");
  });

  test("base 带路径前缀但请求路径不重叠时拼接", () => {
    const url = resolveUrl("https://api.openai.com/v1", "/v2/something", "");
    expect(url).toBe("https://api.openai.com/v1/v2/something");
  });

  test("base 路径与请求路径完全相同", () => {
    const url = resolveUrl("https://api.openai.com/v1", "/v1", "");
    expect(url).toBe("https://api.openai.com/v1");
  });

  test("base 无路径", () => {
    const url = resolveUrl("https://api.openai.com", "/v1/chat/completions", "?stream=true");
    expect(url).toBe("https://api.openai.com/v1/chat/completions?stream=true");
  });

  test("base 带子路径且请求路径有更深路径", () => {
    const url = resolveUrl("https://proxy.example.com/api", "/api/v1/messages", "");
    expect(url).toBe("https://proxy.example.com/api/v1/messages");
  });
});

describe("headersForLog", () => {
  test("完整保留 authorization 和 api-key", () => {
    const h = new Headers({
      authorization: "Bearer sk-very-long-secret-key-1234567890",
      "x-api-key": "ant-very-long-secret-key-1234567890",
      "content-type": "application/json",
    });
    const safe = headersForLog(h);
    expect(safe["authorization"]).toBe(h.get("authorization")!);
    expect(safe["x-api-key"]).toBe(h.get("x-api-key")!);
    expect(safe["content-type"]).toBe("application/json");
  });

  test("完整保留 cookie", () => {
    const h = new Headers({ cookie: "session=abcdef1234567890" });
    const safe = headersForLog(h);
    expect(safe["cookie"]).toBe("session=abcdef1234567890");
  });

  test("普通 header 不脱敏", () => {
    const h = new Headers({ "content-type": "application/json", accept: "*/*" });
    const safe = headersForLog(h);
    expect(safe["content-type"]).toBe("application/json");
    expect(safe["accept"]).toBe("*/*");
  });

  test("完整保留 api-key", () => {
    const h = new Headers({ "api-key": "secret-value-12345" });
    const safe = headersForLog(h);
    expect(safe["api-key"]).toBe("secret-value-12345");
  });
});

// ---------- createHandler 路由 ----------

describe("createHandler 路由", () => {
  let handler: ReturnType<typeof createHandler>;

  beforeEach(() => {
    handler = createHandler(makeTestConfig());
    resetReqCounter();
  });

  test("GET / 返回路由信息", async () => {
    const res = handler(new Request("http://localhost:3456/"));
    const body = await res.json();
    expect(body.gateway).toBe("llm-gateway");
    expect(body.routes["/v1/*"]).toContain("api.openai.com");
    expect(body.routes["/v1/messages"]).toContain("api.anthropic.com");
    expect(body.routes["/health"]).toBe("health check");
  });

  test("GET /health 返回健康状态", async () => {
    const res = handler(new Request("http://localhost:3456/health"));
    const body = await res.json();
    expect(body.ok).toBe(true);
    expect(body.ts).toBeTruthy();
  });

  test("未知路径返回 404", () => {
    const res = handler(new Request("http://localhost:3456/unknown"));
    expect(res.status).toBe(404);
  });

  test("OpenAI 路由匹配 /v1/chat/completions", async () => {
    const originalFetch = globalThis.fetch;
    globalThis.fetch = mock(async () => {
      return new Response(JSON.stringify({ id: "chatcmpl-123", choices: [] }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });

    try {
      const res = await handler(
        new Request("http://localhost:3456/v1/chat/completions", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ model: "gpt-4", messages: [] }),
        }),
      );
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.id).toBe("chatcmpl-123");
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("Anthropic 路由匹配 /v1/messages", async () => {
    const originalFetch = globalThis.fetch;
    globalThis.fetch = mock(async () => {
      return new Response(JSON.stringify({ id: "msg-123", content: [] }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });

    try {
      const res = await handler(
        new Request("http://localhost:3456/v1/messages", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ model: "claude-3", messages: [] }),
        }),
      );
      expect(res.status).toBe(200);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("/v1/models 路由到 OpenAI", async () => {
    const originalFetch = globalThis.fetch;
    globalThis.fetch = mock(async () => {
      return new Response(JSON.stringify({ data: [] }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });

    try {
      const res = await handler(new Request("http://localhost:3456/v1/models"));
      expect(res.status).toBe(200);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("/v1/responses 路由到 OpenAI", async () => {
    const originalFetch = globalThis.fetch;
    globalThis.fetch = mock(async () => {
      return new Response(JSON.stringify({ id: "resp-123" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });

    try {
      const res = await handler(
        new Request("http://localhost:3456/v1/responses", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ model: "gpt-4" }),
        }),
      );
      expect(res.status).toBe(200);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("/v1/* 通配路由覆盖非白名单路径", async () => {
    const originalFetch = globalThis.fetch;
    globalThis.fetch = mock(async () => {
      return new Response(JSON.stringify({ data: [] }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });

    try {
      // 之前 404 的路径现在应该被代理
      const res = await handler(
        new Request("http://localhost:3456/v1/embeddings", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ model: "text-embedding-3-small", input: "hi" }),
        }),
      );
      expect(res.status).toBe(200);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
});

// ---------- proxyRequest 核心逻辑 ----------

describe("proxyRequest", () => {
  beforeEach(() => {
    resetReqCounter();
  });

  test("upstream 不可达时返回 502", async () => {
    const config = makeTestConfig({ logLevel: "none" });
    const originalFetch = globalThis.fetch;

    globalThis.fetch = mock(async () => {
      throw new Error("Connection refused");
    });

    try {
      const res = await proxyRequest(
        config,
        "[openai]",
        config.openaiBase,
        "/v1/chat/completions",
        new Request("http://localhost:3456/v1/chat/completions", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ model: "gpt-4" }),
        }),
      );
      expect(res.status).toBe(502);
      const body = await res.json();
      expect(body.error).toBe("upstream_fetch_failed");
      expect(body.detail).toContain("Connection refused");
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("客户端 header 原样透传到上游", async () => {
    const config = makeTestConfig({ logLevel: "none" });
    let capturedHeaders: Headers | null = null;
    const originalFetch = globalThis.fetch;

    globalThis.fetch = mock(async (_url: string, opts: any) => {
      capturedHeaders = opts.headers as Headers;
      return new Response(JSON.stringify({ ok: true }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });

    try {
      await proxyRequest(
        config,
        "[openai]",
        config.openaiBase,
        "/v1/chat/completions",
        new Request("http://localhost:3456/v1/chat/completions", {
          method: "POST",
          headers: {
            "content-type": "application/json",
            authorization: "Bearer sk-my-key",
            "x-custom": "hello",
          },
          body: JSON.stringify({ model: "gpt-4" }),
        }),
      );
      expect(capturedHeaders).not.toBeNull();
      expect(capturedHeaders!.get("authorization")).toBe("Bearer sk-my-key");
      expect(capturedHeaders!.get("x-custom")).toBe("hello");
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("Anthropic 客户端发送的 x-api-key 原样透传", async () => {
    const config = makeTestConfig({ logLevel: "none" });
    let capturedHeaders: Headers | null = null;
    const originalFetch = globalThis.fetch;

    globalThis.fetch = mock(async (_url: string, opts: any) => {
      capturedHeaders = opts.headers as Headers;
      return new Response(JSON.stringify({ ok: true }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });

    try {
      await proxyRequest(
        config,
        "[anthropic]",
        config.anthropicBase,
        "/v1/messages",
        new Request("http://localhost:3456/v1/messages", {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "x-api-key": "ant-my-key",
            "anthropic-version": "2023-06-01",
          },
          body: JSON.stringify({ model: "claude-3" }),
        }),
      );
      expect(capturedHeaders).not.toBeNull();
      expect(capturedHeaders!.get("x-api-key")).toBe("ant-my-key");
      expect(capturedHeaders!.get("anthropic-version")).toBe("2023-06-01");
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("流式响应直接透传", async () => {
    const config = makeTestConfig({ logLevel: "none" });
    const originalFetch = globalThis.fetch;
    const streamBody = "data: {\"content\":\"hello\"}\n\ndata: [DONE]\n\n";

    globalThis.fetch = mock(async () => {
      return new Response(streamBody, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      });
    });

    try {
      const res = await proxyRequest(
        config,
        "[openai]",
        config.openaiBase,
        "/v1/chat/completions",
        new Request("http://localhost:3456/v1/chat/completions", {
          method: "POST",
          body: JSON.stringify({ model: "gpt-4", stream: true }),
        }),
      );
      expect(res.status).toBe(200);
      expect(res.headers.get("content-type")).toBe("text/event-stream");
      const text = await res.text();
      expect(text).toBe(streamBody);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("GET 请求不发送 body", async () => {
    const config = makeTestConfig({ logLevel: "none" });
    let capturedBody: string | undefined;
    const originalFetch = globalThis.fetch;

    globalThis.fetch = mock(async (_url: string, opts: any) => {
      capturedBody = opts.body;
      return new Response(JSON.stringify({ data: [] }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });

    try {
      await proxyRequest(
        config,
        "[openai]",
        config.openaiBase,
        "/v1/models",
        new Request("http://localhost:3456/v1/models", { method: "GET" }),
      );
      expect(capturedBody).toBeUndefined();
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("上游 header 全量透传", async () => {
    const config = makeTestConfig({ logLevel: "none" });
    const originalFetch = globalThis.fetch;

    globalThis.fetch = mock(async () => {
      return new Response("{}", {
        status: 200,
        headers: {
          "content-type": "application/json",
          "x-custom": "value",
        },
      });
    });

    try {
      const res = await proxyRequest(
        config,
        "[openai]",
        config.openaiBase,
        "/v1/models",
        new Request("http://localhost:3456/v1/models"),
      );
      expect(res.headers.get("x-custom")).toBe("value");
      expect(res.headers.get("content-type")).toBe("application/json");
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
});

// ---------- 集成测试：用 Bun.serve 启动真实服务 ----------

describe("集成测试", () => {
  const testPort = 13456;

  test("health endpoint 通过 HTTP 服务可访问", async () => {
    const config = makeTestConfig({ port: testPort, logLevel: "none" });
    const server = Bun.serve({ port: testPort, fetch: createHandler(config) });

    try {
      const res = await fetch(`http://localhost:${testPort}/health`);
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.ok).toBe(true);
    } finally {
      server.stop();
    }
  });

  test("根路由通过 HTTP 服务可访问", async () => {
    const config = makeTestConfig({ port: testPort + 1, logLevel: "none" });
    const server = Bun.serve({ port: testPort + 1, fetch: createHandler(config) });

    try {
      const res = await fetch(`http://localhost:${testPort + 1}/`);
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.gateway).toBe("llm-gateway");
    } finally {
      server.stop();
    }
  });

  test("代理转发到上游并返回结果", async () => {
    const upstreamPort = testPort + 2;
    const upstreamServer = Bun.serve({
      port: upstreamPort,
      fetch: (req) => {
        const url = new URL(req.url);
        if (url.pathname === "/v1/chat/completions") {
          return Response.json({
            id: "chatcmpl-test",
            object: "chat.completion",
            choices: [{ message: { role: "assistant", content: "Hello!" } }],
          });
        }
        return new Response("Not Found", { status: 404 });
      },
    });

    const config = makeTestConfig({
      port: testPort + 3,
      openaiBase: `http://localhost:${upstreamPort}`,
      logLevel: "none",
    });
    const gatewayServer = Bun.serve({ port: testPort + 3, fetch: createHandler(config) });

    try {
      const res = await fetch(`http://localhost:${testPort + 3}/v1/chat/completions`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ model: "gpt-4", messages: [{ role: "user", content: "Hi" }] }),
      });
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.id).toBe("chatcmpl-test");
      expect(body.choices[0].message.content).toBe("Hello!");
    } finally {
      gatewayServer.stop();
      upstreamServer.stop();
    }
  });

  test("客户端 authorization header 透传到上游", async () => {
    const upstreamPort = testPort + 4;
    let receivedAuth: string | null = null;
    const upstreamServer = Bun.serve({
      port: upstreamPort,
      fetch: (req) => {
        receivedAuth = req.headers.get("authorization");
        return Response.json({ ok: true });
      },
    });

    const config = makeTestConfig({
      port: testPort + 5,
      openaiBase: `http://localhost:${upstreamPort}`,
      logLevel: "none",
    });
    const gatewayServer = Bun.serve({ port: testPort + 5, fetch: createHandler(config) });

    try {
      await fetch(`http://localhost:${testPort + 5}/v1/chat/completions`, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          authorization: "Bearer sk-client-key",
        },
        body: JSON.stringify({ model: "gpt-4" }),
      });
      expect(receivedAuth).toBe("Bearer sk-client-key");
    } finally {
      gatewayServer.stop();
      upstreamServer.stop();
    }
  });

  test("客户端 x-api-key header 透传到上游", async () => {
    const upstreamPort = testPort + 6;
    let receivedApiKey: string | null = null;
    const upstreamServer = Bun.serve({
      port: upstreamPort,
      fetch: (req) => {
        receivedApiKey = req.headers.get("x-api-key");
        return Response.json({ ok: true });
      },
    });

    const config = makeTestConfig({
      port: testPort + 7,
      anthropicBase: `http://localhost:${upstreamPort}`,
      logLevel: "none",
    });
    const gatewayServer = Bun.serve({ port: testPort + 7, fetch: createHandler(config) });

    try {
      await fetch(`http://localhost:${testPort + 7}/v1/messages`, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-api-key": "ant-client-key",
        },
        body: JSON.stringify({ model: "claude-3" }),
      });
      expect(receivedApiKey).toBe("ant-client-key");
    } finally {
      gatewayServer.stop();
      upstreamServer.stop();
    }
  });

  test("上游不可达时返回 502", async () => {
    const config = makeTestConfig({
      port: testPort + 8,
      openaiBase: "http://localhost:19999",
      logLevel: "none",
    });
    const gatewayServer = Bun.serve({ port: testPort + 8, fetch: createHandler(config) });

    try {
      const res = await fetch(`http://localhost:${testPort + 8}/v1/chat/completions`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ model: "gpt-4" }),
      });
      expect(res.status).toBe(502);
      const body = await res.json();
      expect(body.error).toBe("upstream_fetch_failed");
    } finally {
      gatewayServer.stop();
    }
  });

  test("流式响应透传", async () => {
    const upstreamPort = testPort + 9;
    const streamContent = "data: {\"content\":\"hi\"}\n\ndata: [DONE]\n\n";
    const upstreamServer = Bun.serve({
      port: upstreamPort,
      fetch: () => {
        return new Response(streamContent, {
          headers: { "content-type": "text/event-stream" },
        });
      },
    });

    const config = makeTestConfig({
      port: testPort + 10,
      openaiBase: `http://localhost:${upstreamPort}`,
      logLevel: "none",
    });
    const gatewayServer = Bun.serve({ port: testPort + 10, fetch: createHandler(config) });

    try {
      const res = await fetch(`http://localhost:${testPort + 10}/v1/chat/completions`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ model: "gpt-4", stream: true }),
      });
      expect(res.status).toBe(200);
      expect(res.headers.get("content-type")).toBe("text/event-stream");
      const text = await res.text();
      expect(text).toBe(streamContent);
    } finally {
      gatewayServer.stop();
      upstreamServer.stop();
    }
  });
});
