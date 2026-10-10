import { afterAll, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { instantiatePeriWasm, loadPeriWasm, type PeriWasmModule } from "../src/wasm/loader";

const root = mkdtempSync(join(tmpdir(), "peri-wasm-loader-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

function moduleUrl(name: string, exportsEnvironment = true): URL {
  const source = `export default async function(options) {
    const module = { PeriWasmAcp: { start() {} } ${exportsEnvironment ? ", ENV: {}" : ""} };
    for (const callback of options.preRun) callback(module);
    return module;
  } // ${name}`;
  const path = join(root, `${name}.mjs`);
  writeFileSync(path, source);
  return pathToFileURL(path);
}

const injectionUrl = moduleUrl("injection");
const conflictUrl = moduleUrl("conflict");
const invalidUrl = moduleUrl("invalid");
const missingEnvironmentUrl = moduleUrl("missing-env", false);

test("独立 WASM 实例不共享相同 URL 的环境或缓存", async () => {
  const first = await instantiatePeriWasm({ moduleUrl: injectionUrl, env: { INSTANCE: "first" } });
  const second = await instantiatePeriWasm({ moduleUrl: injectionUrl, env: { INSTANCE: "second" } });
  expect(first).not.toBe(second);
  expect(first.ENV).toEqual({ INSTANCE: "first" });
  expect(second.ENV).toEqual({ INSTANCE: "second" });
});

test("注入 factory 不加载文件并保留 preRun 与环境快照", async () => {
  const env = { INSTANCE: "original" };
  let hooks = 0;
  const loading = instantiatePeriWasm({
    moduleUrl: "file:///missing-peri-artifact.js", env,
    moduleOptions: { preRun: () => { hooks++; } },
    moduleFactory: async (options) => {
      await Promise.resolve();
      const module: PeriWasmModule = { ENV: {}, PeriWasmAcp: { start: async () => { throw new Error("unused"); } } };
      for (const hook of options.preRun as Array<(module: PeriWasmModule) => void>) hook(module);
      return module;
    },
  });
  env.INSTANCE = "changed";
  expect((await loading).ENV).toEqual({ INSTANCE: "original" });
  expect(hooks).toBe(1);
});

test("注入 factory 时不解析 moduleUrl，specifier 不可解析也能启动", async () => {
  // peri-cf 形态：宿主只给 moduleFactory；workerd 的 import.meta.url 是 undefined。
  // 不可解析的 specifier 证明这条路径根本不解析产物 URL（修复前会抛 TypeError: Invalid URL）。
  let hooks = 0;
  const module = await instantiatePeriWasm({
    moduleUrl: "http://[",
    env: { INSTANCE: "factory-only" },
    moduleOptions: { preRun: () => { hooks++; } },
    moduleFactory: async (options) => {
      const value: PeriWasmModule = { ENV: {}, PeriWasmAcp: { start: async () => { throw new Error("unused"); } } };
      for (const hook of options.preRun as Array<(module: PeriWasmModule) => void>) hook(value);
      return value;
    },
  });
  expect(module.ENV).toEqual({ INSTANCE: "factory-only" });
  expect(hooks).toBe(1);
});

test("注入 factory 且不给 moduleUrl 时不依赖 import.meta.url", async () => {
  const module = await instantiatePeriWasm({ moduleFactory: async () => ({
    PeriWasmAcp: { start: async () => { throw new Error("unused"); } },
  }) });
  expect(module.PeriWasmAcp.start).toBeFunction();
});

test("缺少 factory 且产物 URL 不可解析时给出可操作的错误", async () => {
  const failure = await instantiatePeriWasm({ moduleUrl: "http://[" }).catch((error) => error);
  expect(failure).toBeInstanceOf(Error);
  expect(failure.message).toContain('Cannot resolve the Peri WASM artifact URL from "http://["');
  expect(failure.message).toContain("moduleFactory");
  expect(failure.cause).toBeInstanceOf(TypeError);
});

test("非法 preRun 不被静默丢弃", async () => {
  await expect(instantiatePeriWasm({ moduleUrl: injectionUrl, moduleOptions: { preRun: [42] } }))
    .rejects.toThrow("Invalid WASM preRun hooks");
});

test("WASM loader injects a copy of deployment environment before startup", async () => {
  const url = injectionUrl;
  const env = { LANGFUSE_PUBLIC_KEY: "pk-test", LANGFUSE_SECRET_KEY: "sk-test" };
  const loading = loadPeriWasm(url, env);
  env.LANGFUSE_SECRET_KEY = "changed";
  const module = await loading;
  expect(module.ENV).toEqual({ LANGFUSE_PUBLIC_KEY: "pk-test", LANGFUSE_SECRET_KEY: "sk-test" });
  expect(await loadPeriWasm(url)).toBe(module);
  expect(await loadPeriWasm(url, { LANGFUSE_SECRET_KEY: "sk-test", LANGFUSE_PUBLIC_KEY: "pk-test" })).toBe(module);
});

test("shared WASM module rejects a different environment without revealing values", async () => {
  const url = conflictUrl;
  const loading = loadPeriWasm(url, { LANGFUSE_SECRET_KEY: "secret-one" });
  expect(() => loadPeriWasm(url, { LANGFUSE_SECRET_KEY: "secret-two" })).toThrow("environment is fixed");
  await loading;
  expect(() => loadPeriWasm(url, {})).toThrow("environment is fixed");
});

test("WASM loader rejects invalid environment names and values", () => {
  for (const env of [{ "": "value" }, { "BAD=NAME": "value" }, { "BAD\0NAME": "value" }, { KEY: "bad\0value" }]) {
    expect(() => loadPeriWasm(invalidUrl, env)).toThrow("Invalid WASM environment variable");
  }
});

test("old WASM artifact reports missing ENV and failed load can retry", async () => {
  const url = missingEnvironmentUrl;
  await expect(loadPeriWasm(url, { LANGFUSE_SECRET_KEY: "secret" })).rejects.toThrow("does not export ENV");
  expect((await loadPeriWasm(url)).PeriWasmAcp).toBeDefined();
});
