import { expect, test } from "bun:test";
import { startPeriWasmHost, PeriWasmHostStartupError, type PeriWasmHostLifecycleEvent } from "../src/wasm/host";
import type { NativeWasmAcp, PeriWasmModule } from "../src/wasm/loader";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function nativePort() {
  const reader = deferred<string | null>();
  let closes = 0;
  let frees = 0;
  const port: NativeWasmAcp = {
    async send() {}, recv: () => reader.promise,
    async close() { closes++; reader.resolve(null); },
    free() { frees++; },
  };
  return { port, reader, get closes() { return closes; }, get frees() { return frees; } };
}

test("ready 不提供停止证明，close 与 free 完成后才宣告 closed", async () => {
  const native = nativePort();
  const closing = deferred<void>();
  const events: PeriWasmHostLifecycleEvent[] = [];
  native.port.close = () => closing.promise;
  const transport = await startPeriWasmHost({ configJson: "{}",
    moduleFactory: async () => ({ PeriWasmAcp: { start: async () => native.port } }),
    onLifecycle: (event) => events.push(event),
  });
  const closed = transport.close();
  expect(await transport.executionStopped()).toBe(false);
  expect(native.frees).toBe(0);
  closing.resolve();
  await closed;
  expect(native.frees).toBe(1);
  expect(await transport.executionStopped()).toBe(true);
  expect(events.map((event) => event.type)).toEqual(["phase", "phase", "ready", "closing", "closed"]);
  native.reader.resolve(null);
});

test("加载前 abort 不调用 factory", async () => {
  let loads = 0;
  const error = await startPeriWasmHost({ configJson: "{}", signal: AbortSignal.abort(),
    moduleFactory: async () => { loads++; throw new Error("不应加载"); },
  }).catch((error) => error);
  expect(error).toBeInstanceOf(PeriWasmHostStartupError);
  expect(await error.cleanup).toEqual({ confirmed: true });
  expect(loads).toBe(0);
});

test("模块加载期间 abort 关闭回调且不启动 native", async () => {
  const controller = new AbortController();
  const loading = deferred<PeriWasmModule>();
  const started = deferred<void>();
  let starts = 0;
  let schedulerClosed = 0;
  const opening = startPeriWasmHost({ configJson: "{}", signal: controller.signal,
    moduleFactory: () => { started.resolve(); return loading.promise; },
    ports: { scheduler: { schedule() {}, cancel() {}, close() { schedulerClosed++; } } },
  }).catch((error) => error);
  await started.promise;
  controller.abort();
  const error = await opening;
  loading.resolve({ PeriWasmAcp: { start: async () => { starts++; return nativePort().port; } } });
  expect(await error.eventualCleanup).toEqual({ confirmed: true });
  expect(starts).toBe(0);
  expect(schedulerClosed).toBeGreaterThan(0);
});

test("native 启动期间 abort，迟到端口必须 close 和 free", async () => {
  const controller = new AbortController();
  const loading = deferred<NativeWasmAcp>();
  const started = deferred<void>();
  const native = nativePort();
  const opening = startPeriWasmHost({ configJson: "{}", signal: controller.signal,
    moduleFactory: async () => ({ PeriWasmAcp: { start: () => { started.resolve(); return loading.promise; } } }),
  }).catch((error) => error);
  await started.promise;
  controller.abort();
  const error = await opening;
  expect(native.closes).toBe(0);
  loading.resolve(native.port);
  expect(await error.eventualCleanup).toEqual({ confirmed: true });
  expect(native.closes).toBe(1);
  expect(native.frees).toBe(1);
});

for (const operation of ["close", "free"] as const) {
  test(`${operation} 失败不能提供终止证明`, async () => {
    const native = nativePort();
    const failure = new Error(`${operation} failed`);
    if (operation === "close") native.port.close = async () => { throw failure; };
    else native.port.free = () => { throw failure; };
    const events: PeriWasmHostLifecycleEvent[] = [];
    const transport = await startPeriWasmHost({ configJson: "{}",
      moduleFactory: async () => ({ PeriWasmAcp: { start: async () => native.port } }),
      onLifecycle: (event) => events.push(event),
    });
    await expect(transport.close()).rejects.toBe(failure);
    expect(await transport.executionStopped()).toBe(false);
    expect(events.some((event) => event.type === "closed")).toBe(false);
    native.reader.resolve(null);
  });
}

test("加载失败仅在自有网络清理后确认", async () => {
  const drained = deferred<void>();
  const error = await startPeriWasmHost({ configJson: "{}",
    moduleFactory: async () => { throw new Error("load failed"); },
    ports: { network: { module: {}, close() {}, drain: () => drained.promise } },
  }).catch((error) => error);
  let confirmed = false;
  void error.eventualCleanup.then(() => { confirmed = true; });
  await Promise.resolve();
  expect(confirmed).toBe(false);
  drained.resolve();
  expect(await error.eventualCleanup).toEqual({ confirmed: true });
});

test("清理超时不算停止，迟到 native 完成后 eventualCleanup 才确认", async () => {
  const controller = new AbortController();
  const loading = deferred<NativeWasmAcp>();
  const started = deferred<void>();
  const native = nativePort();
  const opening = startPeriWasmHost({ configJson: "{}", signal: controller.signal, cleanupTimeoutMs: 5,
    moduleFactory: async () => ({ PeriWasmAcp: { start: () => { started.resolve(); return loading.promise; } } }),
  }).catch((error) => error);
  await started.promise;
  controller.abort();
  const error = await opening;
  expect((await error.cleanup).confirmed).toBe(false);
  loading.resolve(native.port);
  expect(await error.eventualCleanup).toEqual({ confirmed: true });
  expect(native.frees).toBe(1);
});

test("EOF 与显式关闭共享同一次清理", async () => {
  const native = nativePort();
  const transport = await startPeriWasmHost({ configJson: "{}",
    moduleFactory: async () => ({ PeriWasmAcp: { start: async () => native.port } }),
  });
  native.reader.resolve(null);
  await transport.close();
  expect(native.closes).toBe(1);
  expect(native.frees).toBe(1);
  expect(await transport.executionStopped()).toBe(true);
});

test("native 部分启动后拒绝不构成停止证据", async () => {
  const error = await startPeriWasmHost({ configJson: "{}",
    moduleFactory: async () => ({ PeriWasmAcp: { start: async () => { throw new Error("partial startup"); } } }),
  }).catch((error) => error);
  expect((await error.cleanup).confirmed).toBe(false);
  expect((await error.eventualCleanup).confirmed).toBe(false);
});

test("加载失败且端口清理失败不能确认释放", async () => {
  const error = await startPeriWasmHost({ configJson: "{}",
    moduleFactory: async () => { throw new Error("load failed"); },
    ports: { network: { module: {}, close() { throw new Error("destroy failed"); }, async drain() {} } },
  }).catch((error) => error);
  expect((await error.cleanup).confirmed).toBe(false);
});
