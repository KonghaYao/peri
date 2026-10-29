import { describe, expect, test } from "bun:test";
import { OPENUI_ARTIFACT } from "@peri-code/sdk/openui/prompt";
import type { ServerConfig } from "./config";
import { defaultSessionOptions } from "./compose";
import { OPENUI_SYSTEM_PROMPT } from "./openui-instructions";
import { BASE_SYSTEM_INSTRUCTIONS, SYSTEM_INSTRUCTIONS } from "./system-instructions";

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
  mcpServers: [],
  wasmModuleUrl: "file:///tmp/peri-wasm.js",
};

describe("OpenUI system instructions", () => {
  test("完整指令以原有 BASE 开头并包含分隔标题", () => {
    expect(SYSTEM_INSTRUCTIONS.startsWith(BASE_SYSTEM_INSTRUCTIONS)).toBe(true);
    expect(SYSTEM_INSTRUCTIONS).toContain("# Generative UI (OpenUI)");
    expect(SYSTEM_INSTRUCTIONS).toContain(OPENUI_SYSTEM_PROMPT);
  });

  test("每个组件名与四个分组都出现在提示词中", () => {
    for (const name of Object.keys(OPENUI_ARTIFACT.spec.components)) {
      expect(SYSTEM_INSTRUCTIONS).toContain(`${name}(`);
    }
    for (const group of OPENUI_ARTIFACT.spec.componentGroups) {
      expect(SYSTEM_INSTRUCTIONS).toContain(group.name);
    }
  });

  test("包含 root 契约与内置 action step", () => {
    expect(SYSTEM_INSTRUCTIONS).toContain("root = Stack");
    expect(SYSTEM_INSTRUCTIONS).toContain("@ToAssistant");
    expect(SYSTEM_INSTRUCTIONS).toContain("@OpenUrl");
  });

  test("不包含 Cloud 配置块或密钥名", () => {
    expect(SYSTEM_INSTRUCTIONS).not.toContain("]]>openui:config");
    expect(SYSTEM_INSTRUCTIONS).not.toContain("THESYS_API_KEY");
  });

  test("长度低于 40k 预算", () => {
    console.info(`[openui] SYSTEM_INSTRUCTIONS: ${SYSTEM_INSTRUCTIONS.length} chars`);
    expect(SYSTEM_INSTRUCTIONS.length).toBeLessThan(40_000);
  });

  test("新会话默认 options 注入 OpenUI 段", () => {
    const options = defaultSessionOptions(config);
    expect(options.instructions).toBe(SYSTEM_INSTRUCTIONS);
    expect(options.instructions).toContain("# Generative UI (OpenUI)");
    expect(options.instructions).toContain(OPENUI_SYSTEM_PROMPT);
  });
});
