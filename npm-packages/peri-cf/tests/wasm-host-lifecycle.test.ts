import { expect, jest, test } from "bun:test";
import { WasmBudget, WasmCapacityError } from "../worker/wasm/budget";
import { startOwnedWasmHost, WasmStartupError, type NativeWasmHost, type WasmHostOwnership } from "../worker/wasm/lifecycle";
import { WasmResources } from "../worker/wasm/resources";
import { memoryInstance } from "./helpers/wasm";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function fixture() {
  const budget = new WasmBudget();
  const resources = new WasmResources();
  const received = deferred<string | null>();
  let closes = 0;
  let frees = 0;
  const port: NativeWasmHost = {
    send: async () => {}, recv: () => received.promise,
    close: async () => { closes++; }, free: () => { frees++; },
  };
  return { budget, resources, port, received, get closes() { return closes; }, get frees() { return frees; } };
}

test("ready host holds reservation until close and free are confirmed", async () => {
  const state = fixture();
  const transport = await startOwnedWasmHost({ ...state, config: "{}", load: async (owned) => {
    owned.resources.attach(memoryInstance());
    return { PeriWasmAcp: { start: async () => state.port } };
  } });
  expect(() => state.budget.acquire()).toThrow(WasmCapacityError);
  expect(state.resources.snapshot().generationId).toBe(transport.generationId ?? null);
  await transport.close();
  await transport.close();
  expect(state.closes).toBe(1);
  expect(state.frees).toBe(1);
  expect(state.budget.reservedBytes).toBe(0);
  expect(state.resources.snapshot().phase).toBe("closed");
});

test("abort before loading does not allocate a host or consume budget", async () => {
  const state = fixture();
  const controller = new AbortController();
  controller.abort(new Error("cancel before startup"));
  let loads = 0;
  const error = await startOwnedWasmHost({ ...state, config: "{}", signal: controller.signal,
    load: async () => { loads++; throw new Error("must not load"); },
  }).catch((failure) => failure);
  expect(error).toBeInstanceOf(WasmStartupError);
  expect(await error.cleanup).toEqual({ confirmed: true });
  expect(loads).toBe(0);
  expect(state.budget.reservedBytes).toBe(0);
});

test("abort while loading prevents native start and cancels owned callbacks", async () => {
  const state = fixture();
  const controller = new AbortController();
  const loading = deferred<{ PeriWasmAcp: { start(): Promise<NativeWasmHost> } }>();
  const loaded = deferred<void>();
  let ownership!: WasmHostOwnership;
  let starts = 0;
  let callbacks = 0;
  const pending = startOwnedWasmHost({ ...state, config: "{}", signal: controller.signal,
    load: (owned) => { ownership = owned; owned.scheduler.schedule(() => { callbacks++; }); loaded.resolve(); return loading.promise; },
  }).catch((error) => error);
  await loaded.promise;
  controller.abort();
  const error = await pending;
  expect(() => state.budget.acquire()).toThrow(WasmCapacityError);
  loading.resolve({ PeriWasmAcp: { start: async () => { starts++; return state.port; } } });
  expect(await error.cleanup).toEqual({ confirmed: true });
  expect(starts).toBe(0);
  expect(callbacks).toBe(0);
  expect(() => ownership.scheduler.schedule(() => {})).toThrow("closed");
  expect(() => new ownership.network.module.Socket()).toThrow("closed");
  expect(state.budget.reservedBytes).toBe(0);
});

test("late native startup after abort is closed and freed before lease release", async () => {
  const state = fixture();
  const controller = new AbortController();
  const native = deferred<NativeWasmHost>();
  const started = deferred<void>();
  const pending = startOwnedWasmHost({ ...state, config: "{}", signal: controller.signal,
    load: async () => ({ PeriWasmAcp: { start: () => { started.resolve(); return native.promise; } } }),
  }).catch((error) => error);
  await started.promise;
  controller.abort(new Error("cancel pending native start"));
  const error = await pending;
  expect(state.budget.reservedBytes).toBe(67_108_864);
  native.resolve(state.port);
  expect(await error.cleanup).toEqual({ confirmed: true });
  expect(state.closes).toBe(1);
  expect(state.frees).toBe(1);
  expect(state.budget.reservedBytes).toBe(0);
});

test("failed native close retains reservation and original failure", async () => {
  const state = fixture();
  const failure = new Error("native close failed");
  state.port.close = async () => { throw failure; };
  const transport = await startOwnedWasmHost({ ...state, config: "{}",
    load: async () => ({ PeriWasmAcp: { start: async () => state.port } }),
  });
  await expect(transport.close()).rejects.toBe(failure);
  expect(state.frees).toBe(0);
  expect(state.budget.reservedBytes).toBe(67_108_864);
  expect(state.resources.snapshot().phase).toBe("close-unconfirmed");
});

test("failed native free does not claim termination or release reservation", async () => {
  const state = fixture();
  const failure = new Error("native free failed");
  state.port.free = () => { throw failure; };
  const transport = await startOwnedWasmHost({ ...state, config: "{}",
    load: async () => ({ PeriWasmAcp: { start: async () => state.port } }),
  });
  await expect(transport.close()).rejects.toBe(failure);
  expect(state.resources.snapshot().phase).toBe("close-unconfirmed");
  expect(state.budget.reservedBytes).toBe(67_108_864);
});

test("failed module loading releases only after owned cleanup and preserves cause", async () => {
  const state = fixture();
  const failure = new Error("module load failure");
  const error = await startOwnedWasmHost({ ...state, config: "{}", load: async () => { throw failure; } })
    .catch((error) => error);
  expect(error.cause).toBe(failure);
  expect(await error.cleanup).toEqual({ confirmed: true });
  expect(state.budget.reservedBytes).toBe(0);
});

test("timed out startup cleanup stays unconfirmed until the late native is actually closed and freed", async () => {
  jest.useFakeTimers();
  try {
    const state = fixture();
    const controller = new AbortController();
    const native = deferred<NativeWasmHost>();
    const started = deferred<void>();
    const closeStarted = deferred<void>();
    const closed = deferred<void>();
    state.port.close = async () => { closeStarted.resolve(); };
    state.port.free = () => { closed.resolve(); };
    const pending = startOwnedWasmHost({ ...state, config: "{}", signal: controller.signal, cleanupTimeoutMs: 100,
      load: async () => ({ PeriWasmAcp: { start: () => { started.resolve(); return native.promise; } } }),
    }).catch((error) => error);
    await started.promise;
    controller.abort();
    const error = await pending;
    jest.advanceTimersByTime(100);
    const outcome = await error.cleanup;
    expect(outcome.confirmed).toBe(false);
    expect(outcome.error).toBeInstanceOf(Error);
    expect(state.resources.snapshot().phase).toBe("close-unconfirmed");
    expect(state.budget.reservedBytes).toBe(67_108_864);
    native.resolve(state.port);
    await closeStarted.promise;
    await closed.promise;
    expect(state.resources.snapshot().phase).toBe("closed");
    expect(state.budget.reservedBytes).toBe(0);
    expect(await error.eventualCleanup).toEqual({ confirmed: true });
  } finally { jest.useRealTimers(); }
});

test("a native close deadline never frees or releases an unconfirmed host", async () => {
  jest.useFakeTimers();
  try {
    const state = fixture();
    const closeStarted = deferred<void>();
    state.port.close = async () => { closeStarted.resolve(); await new Promise<void>(() => {}); };
    const transport = await startOwnedWasmHost({ ...state, config: "{}", cleanupTimeoutMs: 100,
      load: async () => ({ PeriWasmAcp: { start: async () => state.port } }),
    });
    const closing = transport.close().catch((error) => error);
    await closeStarted.promise;
    jest.advanceTimersByTime(100);
    expect(await closing).toBeInstanceOf(Error);
    expect(state.frees).toBe(0);
    expect(state.resources.snapshot().phase).toBe("close-unconfirmed");
    expect(state.budget.reservedBytes).toBe(67_108_864);
    expect(await transport.executionStopped()).toBe(false);
  } finally { jest.useRealTimers(); }
});

test("native EOF cleanup follows the same reservation release proof as explicit close", async () => {
  const state = fixture();
  const freed = deferred<void>();
  state.port.free = () => { freed.resolve(); };
  const transport = await startOwnedWasmHost({ ...state, config: "{}",
    load: async () => ({ PeriWasmAcp: { start: async () => state.port } }),
  });
  state.received.resolve(null);
  await freed.promise;
  expect(state.budget.reservedBytes).toBe(0);
  expect(state.resources.snapshot().phase).toBe("closed");
  await transport.close();
  expect(state.closes).toBe(1);
});

test("capacity rejection carries confirmed no-allocation cleanup without releasing the live owner", async () => {
  const state = fixture();
  const live = state.budget.acquire();
  let loads = 0;
  const error = await startOwnedWasmHost({ ...state, config: "{}", load: async () => {
    loads++; throw new Error("must not load");
  } }).catch((error) => error);
  expect(error).toBeInstanceOf(WasmStartupError);
  expect(error.cause).toBeInstanceOf(WasmCapacityError);
  expect(await error.cleanup).toEqual({ confirmed: true });
  expect(await error.eventualCleanup).toEqual({ confirmed: true });
  expect(loads).toBe(0);
  expect(state.budget.reservedBytes).toBe(67_108_864);
  live.releaseAfterCleanup();
});

test("partial native start rejection is not treated as proof of a stopped host", async () => {
  const state = fixture();
  const failure = new Error("native start rejected after allocating work");
  const error = await startOwnedWasmHost({ ...state, config: "{}", load: async (owned) => {
    owned.resources.attach(memoryInstance());
    return { PeriWasmAcp: { start: async () => { throw failure; } } };
  } }).catch((error) => error);
  expect(error.cause).toBe(failure);
  expect((await error.cleanup).confirmed).toBe(false);
  expect((await error.eventualCleanup).confirmed).toBe(false);
  expect(state.resources.snapshot()).toMatchObject({ phase: "close-unconfirmed", memory: { observation: "live" } });
  expect(state.budget.reservedBytes).toBe(67_108_864);
  expect(state.frees).toBe(0);
});

test("eventual startup cleanup confirms a native close that finishes beyond the bounded deadline", async () => {
  jest.useFakeTimers();
  try {
    const state = fixture();
    const controller = new AbortController();
    const native = deferred<NativeWasmHost>();
    const started = deferred<void>();
    const closeStarted = deferred<void>();
    const closeGate = deferred<void>();
    state.port.close = async () => { closeStarted.resolve(); await closeGate.promise; };
    const pending = startOwnedWasmHost({ ...state, config: "{}", signal: controller.signal, cleanupTimeoutMs: 100,
      load: async () => ({ PeriWasmAcp: { start: () => { started.resolve(); return native.promise; } } }),
    }).catch((error) => error);
    await started.promise;
    controller.abort();
    const error = await pending;
    native.resolve(state.port);
    await closeStarted.promise;
    jest.advanceTimersByTime(100);
    expect((await error.cleanup).confirmed).toBe(false);
    expect(state.budget.reservedBytes).toBe(67_108_864);
    closeGate.resolve();
    expect(await error.eventualCleanup).toEqual({ confirmed: true });
    expect(state.frees).toBe(1);
    expect(state.budget.reservedBytes).toBe(0);
  } finally { jest.useRealTimers(); }
});
