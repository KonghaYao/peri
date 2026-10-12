/**
 * 宿主注入配置：MCP 条目（插件页维护）在启动时改写为同源 relay 地址，
 * 与 `meta_harness.McpMiddleware` 一起进入 settings。
 */

import { describe, expect, test } from "bun:test";
import type { BrowserConfig } from "../settings/config";
import { harnessConfigJson, MCP_RELAY_PATH } from "./config";

const ORIGIN = "https://peri.example.com";

interface Injected {
  settings: {
    config: {
      meta_harness: Record<string, boolean>;
      mcpServers: Record<string, { url: string; headers?: Record<string, string> }>;
    };
  };
}

function config(mcpServers: BrowserConfig["mcpServers"]): BrowserConfig {
  return {
    machineId: "00000000-0000-4000-8000-000000000000",
    model: { provider: "openai", id: "gpt-test", baseUrl: "https://api.example.com/v1", apiKey: "sk-test" },
    storage: { url: "https://storage.example.com", token: "token" },
    mcpServers,
  };
}

function injected(mcpServers: BrowserConfig["mcpServers"]): Injected {
  return JSON.parse(harnessConfigJson(config(mcpServers), ORIGIN)) as Injected;
}

describe("宿主注入的 MCP 配置", () => {
  test("条目地址改写为同源 relay，请求头文本解析成键值表", () => {
    const { mcpServers, meta_harness } = injected([
      { name: "docs", url: "https://mcp.example.com/sse", headers: "Authorization: Bearer a:b\nX-Tenant: acme" },
    ]).settings.config;

    expect(mcpServers).toEqual({
      docs: {
        url: `${ORIGIN}${MCP_RELAY_PATH}/${encodeURIComponent("https://mcp.example.com/sse")}`,
        headers: { Authorization: "Bearer a:b", "X-Tenant": "acme" },
      },
    });
    // 浏览器部署只开 MCP 工具面，其余能力保持裸 harness 的关闭默认。
    expect(meta_harness.McpMiddleware).toBe(true);
    expect(meta_harness.PermissionMiddleware).toBe(false);
  });

  test("无请求头文本的条目不注入 headers 字段", () => {
    const { mcpServers } = injected([
      { name: "docs", url: "https://mcp.example.com/sse", headers: " \n " },
    ]).settings.config;
    expect(mcpServers.docs).toEqual({
      url: `${ORIGIN}${MCP_RELAY_PATH}/${encodeURIComponent("https://mcp.example.com/sse")}`,
    });
  });

  test("空名与非法的请求头文本按防御性跳过，不影响其它条目", () => {
    const { mcpServers } = injected([
      { name: "  ", url: "https://ignored.example.com/sse", headers: "" },
      { name: "docs", url: " https://mcp.example.com/sse ", headers: "坏头" },
    ]).settings.config;
    expect(Object.keys(mcpServers)).toEqual(["docs"]);
    expect(mcpServers.docs.url)
      .toBe(`${ORIGIN}${MCP_RELAY_PATH}/${encodeURIComponent("https://mcp.example.com/sse")}`);
    expect(mcpServers.docs.headers).toBeUndefined();
  });

  test("未接入 MCP 时注入空表", () => {
    expect(injected([]).settings.config.mcpServers).toEqual({});
  });
});
