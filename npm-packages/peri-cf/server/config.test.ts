import { homedir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import { loadServerConfig, mcpServerDefaults, publicOrigin } from "./config";

const BASE = {
  PERI_MODEL_PROVIDER: "openai",
  PERI_MODEL_ID: "gpt-test",
  PERI_MODEL_BASE_URL: "https://api.example.com/v1",
  PERI_MODEL_API_KEY: "model-secret",
  PERI_STORAGE_URL: "https://store.example.com",
  PERI_STORAGE_TOKEN: "store-secret",
  PERI_MACHINE_ID: "00000000-0000-4000-8000-000000000001",
} as const;

describe("loadServerConfig", () => {
  test("从环境变量装配服务端配置", () => {
    const { config, problems } = loadServerConfig(BASE, "/srv/peri-cf");
    expect(problems).toEqual([]);
    expect(config).toMatchObject({
      hostname: "127.0.0.1",
      port: 8787,
      machineId: BASE.PERI_MACHINE_ID,
      model: {
        provider: "openai",
        tiers: { fable: "gpt-test", opus: "gpt-test", sonnet: "gpt-test", haiku: "gpt-test" },
        apiKey: "model-secret",
      },
      storage: { url: "https://store.example.com", authToken: "store-secret" },
      mcpServers: [],
      wasmModuleUrl: "file:///srv/peri-cf/wasm/generated/peri-wasm.js",
      staticRoot: "/srv/peri-cf/dist/client",
    });
  });

  test("sandbox 默认启用并使用用户目录下的 sandboxesRoot", () => {
    const { config } = loadServerConfig(BASE, "/srv/peri-cf");
    expect(config?.sandbox).toEqual({
      enabled: true,
      root: join(homedir(), ".peri-sandbox", "sandboxes"),
    });
  });

  test("sandbox 可由环境变量禁用或改根目录", () => {
    const { config } = loadServerConfig({
      ...BASE,
      PERI_SANDBOX_ENABLED: "false",
      PERI_SANDBOX_ROOT: "/var/lib/peri-sandboxes",
      PERI_SANDBOX_SETTINGS: "/etc/peri/sandbox.json",
      PERI_SANDBOX_PROFILE: "default",
    }, "/srv/peri-cf");
    expect(config?.sandbox).toEqual({
      enabled: false,
      root: "/var/lib/peri-sandboxes",
      settingsPath: "/etc/peri/sandbox.json",
      profileName: "default",
    });
  });

  test("sandbox 配置非法时拒绝启动", () => {
    const invalidBoolean = loadServerConfig(
      { ...BASE, PERI_SANDBOX_ENABLED: "maybe" },
      "/srv/peri-cf",
    );
    expect(invalidBoolean.config).toBeUndefined();
    expect(invalidBoolean.problems).toContain("PERI_SANDBOX_ENABLED 必须是 true/false");

    const relativeRoot = loadServerConfig(
      { ...BASE, PERI_SANDBOX_ROOT: "relative/sandboxes" },
      "/srv/peri-cf",
    );
    expect(relativeRoot.config).toBeUndefined();
    expect(relativeRoot.problems).toContain("PERI_SANDBOX_ROOT 必须是绝对路径");
  });

  test("远程 MCP 不得占用 sandbox 的 server key", () => {
    const { config, problems } = loadServerConfig({
      ...BASE,
      PERI_MCP_SERVERS: JSON.stringify([{ name: "sandbox", url: "https://mcp.example.com/mcp" }]),
    }, "/srv/peri-cf");
    expect(config).toBeUndefined();
    expect(problems).toContain('PERI_MCP_SERVERS 名称 "sandbox" 保留给 sandbox');
  });

  test("一次性报告缺失项", () => {
    const { config, problems } = loadServerConfig({}, "/srv/peri-cf");
    expect(config).toBeUndefined();
    expect(problems).toEqual(expect.arrayContaining([
      "缺少 PERI_MODEL_ID",
      "缺少 PERI_MODEL_BASE_URL",
      "缺少 PERI_MODEL_API_KEY",
      "缺少 PERI_STORAGE_URL",
      "缺少 PERI_STORAGE_TOKEN",
      "缺少 PERI_MACHINE_ID",
    ]));
  });

  test("拒绝非 http 的存储地址与非法 machineId", () => {
    const { config, problems } = loadServerConfig({
      ...BASE,
      PERI_STORAGE_URL: "libsql://store.example.com",
      PERI_MACHINE_ID: "not-a-uuid",
    }, "/srv/peri-cf");
    expect(config).toBeUndefined();
    expect(problems).toContain("PERI_STORAGE_URL 必须是 http(s) 地址");
    expect(problems).toContain("PERI_MACHINE_ID 必须是 UUID");
  });

  test("远程 MCP 的 token 写成 Authorization 头", () => {
    const { config } = loadServerConfig({
      ...BASE,
      PERI_MCP_SERVERS: JSON.stringify([
        { name: "remote", url: "https://mcp.example.com/mcp", token: "mcp-secret" },
      ]),
    }, "/srv/peri-cf");
    expect(config?.mcpServers).toEqual([{
      name: "remote",
      url: "https://mcp.example.com/mcp",
      headers: [{ name: "Authorization", value: "Bearer mcp-secret" }],
    }]);
  });

  test("MCP 默认声明使用 SDK 的 http 形状", () => {
    const { config } = loadServerConfig({
      ...BASE,
      PERI_MCP_SERVERS: JSON.stringify([{
        name: "remote", url: "https://mcp.example.com/mcp",
        headers: [{ name: "X-Test", value: "yes" }],
      }]),
    }, "/srv/peri-cf");
    expect(mcpServerDefaults(config!)).toEqual({
      remote: {
        type: "http",
        url: "https://mcp.example.com/mcp",
        headers: { "X-Test": "yes" },
      },
    });
  });

  test("非 http 的 MCP 条目拒绝启动", () => {
    const { config, problems } = loadServerConfig({
      ...BASE,
      PERI_MCP_SERVERS: JSON.stringify([{ name: "broken", url: "ftp://mcp.example.com" }]),
    }, "/srv/peri-cf");
    expect(config).toBeUndefined();
    expect(problems).toContain("PERI_MCP_SERVERS[broken] 必须是 http(s) URL");
  });

  test("非法 MCP JSON 不启动服务", () => {
    const { config, problems } = loadServerConfig({ ...BASE, PERI_MCP_SERVERS: "{" }, "/srv/peri-cf");
    expect(config).toBeUndefined();
    expect(problems).toContain("PERI_MCP_SERVERS 不是合法 JSON");
  });

  test("publicOrigin 去掉 userinfo、查询参数与 hash", () => {
    expect(publicOrigin("https://user:pass@store.example.com/db?token=leak#frag"))
      .toBe("https://store.example.com/db");
    expect(publicOrigin("https://mcp.example.com/mcp?key=leak")).toBe("https://mcp.example.com/mcp");
    expect(publicOrigin("not a url")).toBe("");
  });
});
