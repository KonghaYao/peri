import { describe, expect, spyOn, test } from "bun:test";
import {
  TursoStorage,
  type ChatRuntime,
} from "@peri-code/sdk";
import {
  SESSION_CWD,
  WORKSPACE_CWD,
  type ServerConfig,
} from "./config";
import { SYSTEM_INSTRUCTIONS } from "./system-instructions";
import { createSdkChatManager, chatManagerOptions } from "./compose";

const config: ServerConfig = {
  hostname: "127.0.0.1",
  port: 0,
  staticRoot: "/tmp/peri-cf-missing-static",
  machineId: "11111111-1111-4111-8111-111111111111",
  model: {
    provider: "openai",
    tiers: {
      fable: "gpt-fable-test",
      opus: "gpt-opus-test",
      sonnet: "gpt-sonnet-test",
      haiku: "gpt-haiku-test",
    },
    baseUrl: "https://api.example.com/v1",
    apiKey: "model-secret",
  },
  storage: { url: "https://store.example.com", authToken: "store-secret" },
  mcpServers: [{
    name: "tools",
    url: "https://mcp.example.com",
    headers: [{ name: "Authorization", value: "Bearer mcp-token" }],
  }],
  wasmModuleUrl: "file:///tmp/peri-wasm.js",
};

function wasmRuntime(options: ReturnType<typeof chatManagerOptions>): Extract<
  ChatRuntime,
  { kind: "wasm" }
> {
  const { runtime } = options.chatConfig;
  if (runtime.kind !== "wasm") throw new Error("expected wasm runtime");
  return runtime;
}

describe("chatManagerOptions", () => {
  test("把模型、四档 profiles 与工具面装配成 agentConfig", () => {
    const options = chatManagerOptions(config);

    expect(options.agentConfig).toEqual({
      cwd: WORKSPACE_CWD,
      machineId: config.machineId,
      storage: {
        url: config.storage.url,
        authToken: config.storage.authToken,
      },
      settings: {
        config: {
          active_alias: "sonnet",
          providers: [{
            id: "model",
            type: config.model.provider,
            apiKey: config.model.apiKey,
            baseUrl: config.model.baseUrl,
            models: config.model.tiers,
          }],
          profiles: {
            fable: { provider: "model" },
            opus: { provider: "model" },
            sonnet: { provider: "model" },
            haiku: { provider: "model" },
          },
          meta_harness: expect.objectContaining({
            "01_intro": false,
            TodoMiddleware: true,
            HumanInTheLoopMiddleware: true,
            SubAgentMiddleware: true,
            BuiltInSubagents: true,
            McpMiddleware: true,
            ToolSearch: true,
            GoalMiddleware: true,
            WorkflowMiddleware: false,
          }),
        },
      },
    });
  });

  test("storage 使用默认 database 的纯 JSON storageConfig", () => {
    const options = chatManagerOptions(config);

    expect(options.database).toBeInstanceOf(TursoStorage);
    const database = options.database as TursoStorage;
    const agentStorage = (options.agentConfig as { storage: unknown }).storage;

    expect(agentStorage).toEqual(database.storageConfig);
    expect(agentStorage).toEqual({
      url: config.storage.url,
      authToken: config.storage.authToken,
    });
    expect(Object.keys(agentStorage as object).sort()).toEqual(["authToken", "url"]);
    expect(JSON.parse(JSON.stringify(agentStorage))).toEqual(agentStorage);
    expect(agentStorage).not.toHaveProperty("legacyCwd");
    expect(agentStorage).not.toHaveProperty("currentCwd");
  });

  test("sessionInit 注入会话 cwd、系统指令与远程 MCP，kv 使用进程内存储", () => {
    const options = chatManagerOptions(config);

    expect(options.kv).toBeDefined();
    expect(options.chatConfig.sessionInit).toEqual({
      path: SESSION_CWD,
      instructions: SYSTEM_INSTRUCTIONS,
      mcpServers: {
        tools: {
          type: "http",
          url: "https://mcp.example.com",
          headers: { Authorization: "Bearer mcp-token" },
        },
      },
    });
    expect(options.chatConfig.lifecycle).toBeUndefined();
  });

  test("默认只把 LANGFUSE_ 变量交给 WASM，诊断走 [peri-cf] 日志", () => {
    const originalTracing = process.env.LANGFUSE_TRACING;
    const originalSecret = process.env.PERI_SERVER_SECRET;
    process.env.LANGFUSE_TRACING = "true";
    process.env.PERI_SERVER_SECRET = "must-not-leak";
    const logged = spyOn(console, "error").mockImplementation(() => {});
    try {
      const runtime = wasmRuntime(chatManagerOptions(config));

      expect(runtime.moduleUrl).toBe(config.wasmModuleUrl);
      expect(runtime.env).toEqual({ LANGFUSE_TRACING: "true" });

      const diagnostic = new Error("runtime startup failed");
      runtime.onDiagnostic?.("startup", diagnostic);
      expect(logged).toHaveBeenCalledTimes(1);
      expect(logged).toHaveBeenCalledWith(
        "[peri-cf] Peri server runtime diagnostic",
        { phase: "runtime-start", kind: "startup" },
        diagnostic,
      );
    } finally {
      logged.mockRestore();
      if (originalTracing === undefined) delete process.env.LANGFUSE_TRACING;
      else process.env.LANGFUSE_TRACING = originalTracing;
      if (originalSecret === undefined) delete process.env.PERI_SERVER_SECRET;
      else process.env.PERI_SERVER_SECRET = originalSecret;
    }
  });

  test("createSdkChatManager 返回 SDK 默认实现，句柄登记不启动运行后端", () => {
    const manager = createSdkChatManager(config);
    const chat = manager.createChat();

    expect(chat.sessionId).toBeNull();
    expect(chat.bindingRevision).toBe(0);
  });
});
