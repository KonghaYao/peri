import { describe, expect, test } from "bun:test";
import { wasmConfig } from "../worker/wasm/config";
import type { Env } from "../worker/types";

function environment(): Env {
  return {
    APP_AUTH_TOKEN: "app-token-not-for-rust",
    PERI_MACHINE_ID: "11111111-1111-4111-8111-111111111111",
    PERI_STORAGE_URL: "libsql://rust-store.invalid",
    PERI_STORAGE_TOKEN: "rust-store-token",
    MODEL_BASE_URL: "https://model.invalid/v1",
    MODEL_API_KEY: "model-token",
    MODEL_ID: "configured-model",
    CHAT_SESSIONS: {
      idFromName() { throw new Error("DO must not configure Rust storage"); },
      get() { throw new Error("DO must not configure Rust storage"); },
    },
  };
}

describe("WASM ACP startup configuration", () => {
  test("routes Rust storage to external Turso and uses the configured model without forwarding app auth", () => {
    const serialized = wasmConfig(environment());
    const config = JSON.parse(serialized);
    expect(config.cwd).toBe("/workspace");
    expect(config.machineId).toBe(environment().PERI_MACHINE_ID);
    expect(config.storage).toEqual({ url: "libsql://rust-store.invalid", authToken: "rust-store-token" });
    expect(config.settings.config.providers).toEqual([
      { id: "model", type: "openai", apiKey: "model-token", baseUrl: "https://model.invalid/v1" },
    ]);
    const settings = config.settings.config;
    expect(settings.profiles[settings.active_alias]).toEqual({ provider: "model", model: "configured-model" });
    expect(serialized).not.toContain("app-token-not-for-rust");
  });

  test("disables tool, MCP and delegation capabilities in the virtual workspace", () => {
    const harness = JSON.parse(wasmConfig(environment())).settings.config.meta_harness;
    for (const capability of ["McpMiddleware", "ToolSearch", "SubAgentMiddleware", "WorkspaceMiddleware", "HumanInTheLoopMiddleware"]) {
      expect(harness[capability]).toBe(false);
    }
    expect(Object.values(harness).every((value) => value === false)).toBe(true);
  });

  test.each(["PERI_MACHINE_ID", "PERI_STORAGE_URL", "PERI_STORAGE_TOKEN", "MODEL_BASE_URL", "MODEL_API_KEY", "MODEL_ID"] as const)(
    "fails explicitly when %s is missing or empty", (name) => {
      const env = environment();
      delete env[name];
      expect(() => wasmConfig(env)).toThrow(`${name} is not configured`);
      env[name] = "";
      expect(() => wasmConfig(env)).toThrow(`${name} is not configured`);
    },
  );

  test.each(["not-a-uuid", "123", "11111111-1111-4111-8111-11111111111", " 11111111-1111-4111-8111-111111111111"])(
    "rejects malformed deployment identity %s", (machineId) => {
      const env = environment();
      env.PERI_MACHINE_ID = machineId;
      expect(() => wasmConfig(env)).toThrow("PERI_MACHINE_ID must be a persistent deployment UUID");
    },
  );

  test("uses an explicitly configured deployment identity rather than a hardcoded fixture identity", () => {
    const env = environment();
    env.PERI_MACHINE_ID = "22222222-2222-4222-8222-222222222222";
    expect(JSON.parse(wasmConfig(env)).machineId).toBe(env.PERI_MACHINE_ID);
  });
});
