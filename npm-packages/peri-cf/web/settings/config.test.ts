/**
 * 浏览器本地配置（设置页 + 插件页）：MCP 条目的收敛读取、请求头文本解析与校验
 * 决定插件页能否保存、宿主能否启动；另覆盖配置文件经 VFS 的落盘往返。
 */

import "fake-indexeddb/auto";
import { describe, expect, test } from "bun:test";
import { createVfs, type Vfs } from "../vfs";
import {
  configProblems,
  loadBrowserConfig,
  parseMcpHeaders,
  saveBrowserConfig,
  type BrowserConfig,
} from "./config";

let sequence = 0;

/** 只在内存里承载一个文件文本的 VFS 替身：用于只关心收敛与校验的用例。 */
function stubVfs(content: string | null = null): Vfs {
  let current = content;
  return {
    readText: async () => current,
    writeText: async (_path, text) => { current = text; },
    deleteFile: async () => { current = null; },
    list: async () => [],
  };
}

/** 真实 VFS（IndexedDB）：每个用例一棵独立的库。 */
function realVfs(): Vfs {
  sequence += 1;
  return createVfs(`peri-config-test-${sequence}`);
}

function config(overrides: Partial<BrowserConfig> = {}): BrowserConfig {
  return {
    machineId: "00000000-0000-4000-8000-000000000000",
    model: { provider: "openai", id: "gpt-test", baseUrl: "https://api.example.com/v1", apiKey: "sk-test" },
    storage: { url: "https://storage.example.com", token: "token" },
    mcpServers: [],
    ...overrides,
  };
}

describe("MCP 条目读取与请求头解析", () => {
  test("读取配置文件里的 MCP 条目并逐字段收敛", async () => {
    const vfs = stubVfs(JSON.stringify({
      machineId: "00000000-0000-4000-8000-000000000000",
      mcpServers: [
        { name: "docs", url: "https://mcp.example.com/sse", headers: "X-Tenant: acme" },
        { name: 7, url: null },
        "不是对象",
      ],
    }));
    expect((await loadBrowserConfig(vfs)).mcpServers).toEqual([
      { name: "docs", url: "https://mcp.example.com/sse", headers: "X-Tenant: acme" },
      { name: "", url: "", headers: "" },
    ]);
  });

  test("mcpServers 缺失或不是数组时按未接入处理", async () => {
    expect((await loadBrowserConfig(stubVfs(JSON.stringify({ mcpServers: "docs" })))).mcpServers).toEqual([]);
    expect((await loadBrowserConfig(stubVfs(JSON.stringify({})))).mcpServers).toEqual([]);
    expect((await loadBrowserConfig(stubVfs(null))).mcpServers).toEqual([]);
  });

  test("配置文件损坏时按未配置处理，不把原文带进错误面", async () => {
    expect((await loadBrowserConfig(stubVfs("{ 不是 JSON"))).model).toEqual({
      provider: "openai", id: "", baseUrl: "", apiKey: "",
    });
  });

  test("请求头文本按行解析，值中的冒号保留，空行忽略", () => {
    expect(parseMcpHeaders("")).toEqual({});
    expect(parseMcpHeaders("\n  \n")).toEqual({});
    expect(parseMcpHeaders("Authorization: Bearer a:b\nX-Tenant: acme\n"))
      .toEqual({ Authorization: "Bearer a:b", "X-Tenant": "acme" });
  });

  test("缺 Key 的行解析为 null，由校验报出", () => {
    expect(parseMcpHeaders("Authorization: Bearer a\n没有冒号的行")).toBeNull();
    expect(parseMcpHeaders(": 空 Key")).toBeNull();
  });
});

describe("配置落盘（真实 VFS）", () => {
  test("保存后读回逐字段一致", async () => {
    const vfs = realVfs();
    const saved = config({ mcpServers: [{ name: "docs", url: "https://mcp.example.com/sse", headers: "" }] });
    await saveBrowserConfig(vfs, saved);
    expect(await loadBrowserConfig(vfs)).toEqual(saved);
  });

  test("machineId 首次生成后固化，二次读取不变", async () => {
    const vfs = realVfs();
    const first = await loadBrowserConfig(vfs);
    expect(first.machineId).toMatch(/^[0-9a-f-]{36}$/i);
    expect((await loadBrowserConfig(vfs)).machineId).toBe(first.machineId);
  });

  test("空的 VFS 上首次读取即把新生成的 machineId 固化", async () => {
    const vfs = realVfs();
    const machineId = (await loadBrowserConfig(vfs)).machineId;
    expect(JSON.parse((await vfs.readText("/config.json")) ?? "{}").machineId).toBe(machineId);
  });
});

describe("MCP 条目校验", () => {
  test("未接入 MCP 时不影响启动", () => {
    expect(configProblems(config())).toEqual([]);
  });

  test("合法条目放行；名称、URL、请求头逐项报出", () => {
    expect(configProblems(config({ mcpServers: [
      { name: "docs", url: "https://mcp.example.com/sse", headers: "" },
    ] }))).toEqual([]);
    expect(configProblems(config({ mcpServers: [
      { name: "含 空格", url: "libsql://mcp.example.com", headers: "坏头" },
    ] }))).toEqual([
      "MCP #1 名称（仅字母、数字、_ 与 -）",
      "MCP #1 URL（须为 http(s) 地址）",
      "MCP #1 请求头（每行须为 Key: Value）",
    ]);
  });

  test("名称必填且唯一", () => {
    expect(configProblems(config({ mcpServers: [
      { name: " ", url: "https://mcp.example.com/sse", headers: "" },
      { name: "docs", url: "https://mcp.example.com/sse", headers: "" },
      { name: "docs", url: "https://mcp.example.com/sse", headers: "" },
    ] }))).toEqual(["MCP #1 名称", "MCP #3 名称重复"]);
  });
});
