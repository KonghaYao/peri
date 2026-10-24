import { describe, expect, test } from "bun:test";
import { loadServerConfig, publicServerInfo } from "./config";

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
      model: { provider: "openai", id: "gpt-test", apiKey: "model-secret" },
      storage: { url: "https://store.example.com", authToken: "store-secret" },
      mcpServers: [],
      wasmModuleUrl: "file:///srv/peri-cf/wasm/generated/peri-wasm.js",
      staticRoot: "/srv/peri-cf/dist/client",
    });
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
        { name: "workspace", url: "https://mcp.example.com/mcp", token: "mcp-secret" },
      ]),
    }, "/srv/peri-cf");
    expect(config?.mcpServers).toEqual([{
      name: "workspace",
      url: "https://mcp.example.com/mcp",
      headers: [{ name: "Authorization", value: "Bearer mcp-secret" }],
    }]);
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

  test("公开信息去掉 token 与查询参数", () => {
    const { config } = loadServerConfig({
      ...BASE,
      PERI_STORAGE_URL: "https://user:pass@store.example.com/db?token=leak",
      PERI_MCP_SERVERS: JSON.stringify([{ name: "workspace", url: "https://mcp.example.com/mcp?key=leak" }]),
    }, "/srv/peri-cf");
    const info = publicServerInfo(config!);
    const serialized = JSON.stringify(info);
    expect(serialized).not.toContain("secret");
    expect(serialized).not.toContain("leak");
    expect(serialized).not.toContain("user:pass");
    expect(info.storage.url).toBe("https://store.example.com/db");
  });
});
