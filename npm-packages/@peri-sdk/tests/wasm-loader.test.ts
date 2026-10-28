import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { loadWasmArtifact } from "../src/wasm/artifact-cache";
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

/** Empty wasm module: enough to compile, instantiate and compare module identity. */
const emptyWasm = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

interface SharedArtifactProbe extends PeriWasmModule {
  hasInstantiateWasm: boolean;
  shared: { instance: WebAssembly.Instance; module: WebAssembly.Module } | null;
}

/**
 * Minimal Emscripten-shaped glue in its own directory; the loader resolves `peri_wasm.wasm`
 * beside it. `wasm === null` exercises the missing-binary fallback.
 */
function sharedArtifactUrl(name: string, wasm: Uint8Array | null = emptyWasm): URL {
  const directory = join(root, name);
  mkdirSync(directory, { recursive: true });
  const path = join(directory, "peri-wasm.js");
  writeFileSync(path, `export default async function(options) {
    let shared = null;
    if (typeof options.instantiateWasm === "function") {
      shared = await new Promise((resolve) => options.instantiateWasm({},
        (instance, module) => resolve({ instance, module })));
    }
    const module = {
      PeriWasmAcp: { start() {} }, ENV: {}, shared,
      hasInstantiateWasm: typeof options.instantiateWasm === "function",
    };
    for (const callback of options.preRun) callback(module);
    return module;
  } // ${name}`);
  if (wasm !== null) writeFileSync(join(directory, "peri_wasm.wasm"), wasm);
  return pathToFileURL(path);
}

test("共享同一 URL 的多个实例只编译一次产物", async () => {
  const url = sharedArtifactUrl("shared-artifact");
  const first = await instantiatePeriWasm({ moduleUrl: url }) as SharedArtifactProbe;
  const second = await instantiatePeriWasm({ moduleUrl: url }) as SharedArtifactProbe;
  expect(first).not.toBe(second);
  expect(first.hasInstantiateWasm).toBe(true);
  expect(first.shared).not.toBeNull();
  expect(second.shared).not.toBeNull();
  expect(second.shared!.module).toBe(first.shared!.module);
  expect(second.shared!.instance).not.toBe(first.shared!.instance);
});

test("并发启动共享 single-flight 编译", async () => {
  const url = sharedArtifactUrl("concurrent-artifact");
  const [first, second] = await Promise.all([
    instantiatePeriWasm({ moduleUrl: url }),
    instantiatePeriWasm({ moduleUrl: url }),
  ]) as SharedArtifactProbe[];
  expect(first.hasInstantiateWasm).toBe(true);
  expect(second.shared).not.toBeNull();
  expect(second.shared!.module).toBe(first.shared!.module);
});

test("编译失败不缓存，第二次启动会重试", async () => {
  const url = sharedArtifactUrl("broken-artifact", new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0xff, 0xff]));
  const failure = await instantiatePeriWasm({ moduleUrl: url }).catch((error: unknown) => error);
  expect(failure).toBeInstanceOf(WebAssembly.CompileError);

  writeFileSync(join(root, "broken-artifact", "peri_wasm.wasm"), emptyWasm);
  const module = await instantiatePeriWasm({ moduleUrl: url }) as SharedArtifactProbe;
  expect(module.hasInstantiateWasm).toBe(true);
  expect(module.shared!.module).toBeInstanceOf(WebAssembly.Module);
});

test("找不到 .wasm 时回退到工厂自有的加载路径", async () => {
  const url = sharedArtifactUrl("missing-artifact", null);
  const module = await instantiatePeriWasm({ moduleUrl: url }) as SharedArtifactProbe;
  expect(module.hasInstantiateWasm).toBe(false);
  expect(module.shared).toBeNull();
  expect(module.PeriWasmAcp.start).toBeFunction();
});

/** Short enough to keep the regression tests fast; production uses `ARTIFACT_READ_TIMEOUT_MS`. */
const TEST_READ_TIMEOUT_MS = 100;

/** Server whose first request hangs forever; later requests serve a compilable artifact. */
function hangingArtifactServer(): { url: URL; requests: () => number; stop: () => Promise<void> } {
  let requests = 0;
  const server = Bun.serve({
    port: 0,
    fetch() {
      requests++;
      if (requests === 1) return new Promise<Response>(() => {});
      return new Response(emptyWasm);
    },
  });
  return {
    url: new URL(`http://127.0.0.1:${server.port}/peri_wasm.wasm`),
    requests: () => requests,
    stop: () => server.stop(true),
  };
}

test("产物读取超时返回回退并清除缓存，第二次读取会重试", async () => {
  const server = hangingArtifactServer();
  try {
    expect(await loadWasmArtifact(server.url, TEST_READ_TIMEOUT_MS)).toBeNull();
    expect(server.requests()).toBe(1);

    // The timed-out entry is gone: this call issues a new request and compiles the artifact.
    const retried = await loadWasmArtifact(server.url, TEST_READ_TIMEOUT_MS);
    expect(retried).toBeInstanceOf(WebAssembly.Module);
    expect(server.requests()).toBe(2);
  } finally {
    await server.stop();
  }
});

test("并发等待同一次挂起读取时都回退，且不残留缓存", async () => {
  const server = hangingArtifactServer();
  try {
    const first = loadWasmArtifact(server.url, TEST_READ_TIMEOUT_MS);
    const second = loadWasmArtifact(server.url, TEST_READ_TIMEOUT_MS);
    expect(second).toBe(first);
    expect(await Promise.all([first, second])).toEqual([null, null]);
    expect(server.requests()).toBe(1);

    expect(await loadWasmArtifact(server.url, TEST_READ_TIMEOUT_MS)).toBeInstanceOf(WebAssembly.Module);
    expect(server.requests()).toBe(2);
  } finally {
    await server.stop();
  }
});

/** A glue module that triggers the shared-instantiation hook without waiting for its callback. */
function instantiateHookUrl(name: string, wasm: Uint8Array = emptyWasm): URL {
  const directory = join(root, name);
  mkdirSync(directory, { recursive: true });
  writeFileSync(join(directory, "peri_wasm.wasm"), wasm);
  const path = join(directory, "peri-wasm.js");
  writeFileSync(path, `export default async function(options) {
    options.instantiateWasm({}, () => { throw new Error("callback boom"); });
    return { PeriWasmAcp: { start() {} }, ENV: {} };
  } // ${name}`);
  return pathToFileURL(path);
}

function captureProcessSignals(): { errors: unknown[][]; unhandled: unknown[]; restore: () => void } {
  const errors: unknown[][] = [];
  const unhandled: unknown[] = [];
  const originalError = console.error;
  const onUnhandled = (reason: unknown) => { unhandled.push(reason); };
  console.error = (...args: unknown[]) => { errors.push(args); };
  process.on("unhandledRejection", onUnhandled);
  return {
    errors,
    unhandled,
    restore() {
      console.error = originalError;
      process.off("unhandledRejection", onUnhandled);
    },
  };
}

test("successCallback 同步抛错只记录日志，不产生未处理拒绝", async () => {
  const signals = captureProcessSignals();
  try {
    const module = await instantiatePeriWasm({ moduleUrl: instantiateHookUrl("callback-throw") });
    expect(module.PeriWasmAcp.start).toBeFunction();
    // Let the asynchronous instantiation chain run its callback and catch it.
    await Bun.sleep(50);
    expect(signals.unhandled).toEqual([]);
    expect(signals.errors.some((args) => String(args[0]).includes("instantiation callback failed"))).toBe(true);
  } finally {
    signals.restore();
  }
});

/**
 * Valid module that imports `env.boom`; instantiating it with no imports rejects asynchronously.
 * [type: () -> ()], [import env.boom], no definitions or exports.
 */
const importingWasm = new Uint8Array([
  0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
  0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
  0x02, 0x0c, 0x01, 0x03, 0x65, 0x6e, 0x76, 0x04, 0x62, 0x6f, 0x6f, 0x6d, 0x00, 0x00,
]);

test("WebAssembly.instantiate 异步拒绝只记录日志，不产生未处理拒绝", async () => {
  const signals = captureProcessSignals();
  try {
    const directory = join(root, "instantiate-reject");
    mkdirSync(directory, { recursive: true });
    writeFileSync(join(directory, "peri_wasm.wasm"), importingWasm);
    writeFileSync(join(directory, "peri-wasm.js"), `export default async function(options) {
      options.instantiateWasm({}, () => {});
      return { PeriWasmAcp: { start() {} }, ENV: {} };
    } // instantiate-reject`);
    const module = await instantiatePeriWasm({ moduleUrl: pathToFileURL(join(directory, "peri-wasm.js")) });
    expect(module.PeriWasmAcp.start).toBeFunction();
    await Bun.sleep(50);
    expect(signals.unhandled).toEqual([]);
    expect(signals.errors.some((args) => String(args[0]).includes("instance creation failed"))).toBe(true);
  } finally {
    signals.restore();
  }
});
