import { describe, expect, spyOn, test } from "bun:test";
import {
  TursoStorage,
  type ChatRuntime,
} from "@peri-code/sdk";
import {
  WORKSPACE_CWD,
  type ServerConfig,
} from "./config";
import { SYSTEM_INSTRUCTIONS } from "./system-instructions";
import { chatManagerOptions, defaultAgentConfig, defaultWorkspace } from "./compose";

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
  sandbox: { enabled: false, root: "/tmp/peri-sandbox" },
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
  test("defaultWorkspace 固定宿主与运行时根为 /workspace", () => {
    expect(defaultWorkspace(config)).toEqual({
      id: "default",
      hostPath: WORKSPACE_CWD,
      runtimePath: WORKSPACE_CWD,
    });
  });

  test("defaultAgentConfig 把模型、四档 profiles 与工具面装配成 per-chat agent", () => {
    const agent = defaultAgentConfig(config);

    expect(agent).toEqual({
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
            "13_skills": true,
            SkillsMiddleware: true,
            SkillPreloadMiddleware: true,
            AgentsMdMiddleware: true,
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
    // Store 与机器身份只属于 Manager，per-chat 声明会被 SDK 拒绝。
    expect(agent).not.toHaveProperty("storage");
    expect(agent).not.toHaveProperty("machineId");
    // cwd 由 SDK 从 workspace.runtimePath 派生，不能与 workspace 漂移。
    expect(agent).not.toHaveProperty("cwd");
  });

  test("storage 由 Manager store 使用默认 database 的纯 JSON storageConfig", () => {
    const options = chatManagerOptions(config);

    expect(options.database).toBeInstanceOf(TursoStorage);
    const database = options.database as TursoStorage;
    const agentStorage = options.store.storage;

    expect(agentStorage).toEqual(database.storageConfig);
    expect(agentStorage).toEqual({
      url: config.storage.url,
      authToken: config.storage.authToken,
    });
    expect(options.store.machineId).toBe(config.machineId);
    expect(Object.keys(agentStorage as object).sort()).toEqual(["authToken", "url"]);
    expect(JSON.parse(JSON.stringify(agentStorage))).toEqual(agentStorage);
    expect(agentStorage).not.toHaveProperty("legacyCwd");
    expect(agentStorage).not.toHaveProperty("currentCwd");
    expect(options).not.toHaveProperty("agentConfig");
  });

  test("session 注入系统指令与远程 MCP，cwd 留给 workspace 派生", () => {
    const options = chatManagerOptions(config);

    expect(options.kv).toBeDefined();
    expect(options.chatConfig.session).toEqual({
      instructions: SYSTEM_INSTRUCTIONS,
      mcpServers: {
        tools: {
          type: "http",
          url: "https://mcp.example.com",
          headers: { Authorization: "Bearer mcp-token" },
        },
      },
    });
    expect(options.chatConfig.session).not.toHaveProperty("path");
    expect(options.chatConfig.lifecycle).toBeUndefined();
  });

  test("只把 Rust 读取的 LANGFUSE_ 键交给 WASM，诊断走 [peri-cf] 日志", () => {
    const originalLangfuse = Object.entries(process.env).filter(
      (entry): entry is [string, string] =>
        entry[0].startsWith("LANGFUSE_") && entry[1] !== undefined,
    );
    const originalSecret = process.env.PERI_SERVER_SECRET;
    const clearLangfuse = () => {
      for (const key of Object.keys(process.env)) {
        if (key.startsWith("LANGFUSE_")) delete process.env[key];
      }
    };

    clearLangfuse();
    process.env.LANGFUSE_PUBLIC_KEY = "pk-test";
    process.env.LANGFUSE_SECRET_KEY = "sk-test";
    process.env.LANGFUSE_BASE_URL = "https://langfuse.example.com";
    process.env.LANGFUSE_TRACE_SAMPLING = "0.25";
    process.env.LANGFUSE_TRACING = "true";
    process.env.PERI_SERVER_SECRET = "must-not-leak";
    const logged = spyOn(console, "error").mockImplementation(() => {});
    try {
      const runtime = wasmRuntime(chatManagerOptions(config));

      expect(runtime.moduleUrl).toBe(config.wasmModuleUrl);
      // 只转发允许列表中的已设置键：未设置的键被跳过，LANGFUSE_TRACING 与
      // PERI_SERVER_SECRET 不进入 WASM。
      expect(runtime.env).toEqual({
        LANGFUSE_PUBLIC_KEY: "pk-test",
        LANGFUSE_SECRET_KEY: "sk-test",
        LANGFUSE_BASE_URL: "https://langfuse.example.com",
        LANGFUSE_TRACE_SAMPLING: "0.25",
      });

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
      clearLangfuse();
      for (const [key, value] of originalLangfuse) process.env[key] = value;
      if (originalSecret === undefined) delete process.env.PERI_SERVER_SECRET;
      else process.env.PERI_SERVER_SECRET = originalSecret;
    }
  });
});
