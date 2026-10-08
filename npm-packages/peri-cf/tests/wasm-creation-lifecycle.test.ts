import { expect, jest, test } from "bun:test";
import { createPeriSession } from "../worker/chat/creation";
import { WasmBudget, WasmCapacityError } from "../worker/wasm/budget";
import { startOwnedWasmHost, type NativeWasmHost } from "../worker/wasm/lifecycle";
import type { Env, StartTransport } from "../worker/types";
import { HTTPException } from "hono/http-exception";
import { errorResponse } from "../worker/api/http";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}

function creationHost() {
  const sessionId = "32102c8b-2b8a-471b-9bb1-2e9b68ce8f49";
  let reader = deferred<string | null>();
  let closes = 0;
  let frees = 0;
  const port: NativeWasmHost = {
    send: async (frame) => {
      const request = JSON.parse(frame);
      const result = request.method === "initialize" ? { protocolVersion: 1 }
        : request.method === "session/new" ? { sessionId }
          : { sessionId, title: request.params.title };
      const current = reader;
      reader = deferred();
      current.resolve(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }));
    },
    recv: () => reader.promise,
    close: async () => { closes++; reader.resolve(null); },
    free: () => { frees++; },
  };
  return { port, sessionId, get closes() { return closes; }, get frees() { return frees; } };
}

test("creation uses real ACP wire and confirms ownership cleanup before returning the session", async () => {
  const state = creationHost();
  const budget = new WasmBudget();
  const start: StartTransport = (_env, resources, signal) => startOwnedWasmHost({
    config: "{}", resources, signal, budget,
    load: async () => ({ PeriWasmAcp: { start: async () => state.port } }),
  });
  const created = await createPeriSession({} as Env, "new chat", start, () => {});
  expect(created).toBe(state.sessionId);
  expect(state.closes).toBe(1);
  expect(state.frees).toBe(1);
  expect(budget.reservedBytes).toBe(0);
});

test("creation startup deadline aborts owned startup and observes late confirmed cleanup", async () => {
  jest.useFakeTimers();
  try {
    const state = creationHost();
    const budget = new WasmBudget();
    const started = deferred<void>();
    const native = deferred<NativeWasmHost>();
    const observed: Promise<unknown>[] = [];
    let startupSignal: AbortSignal | undefined;
    const start: StartTransport = (_env, resources, signal) => {
      startupSignal = signal;
      return startOwnedWasmHost({ config: "{}", resources, signal, budget,
        load: async () => ({ PeriWasmAcp: { start: () => { started.resolve(); return native.promise; } } }),
      });
    };
    const creating = createPeriSession({} as Env, "timed out chat", start, (promise) => { observed.push(promise); })
      .catch((error) => error);
    await started.promise;
    jest.advanceTimersByTime(20_000);
    const error = await creating;
    expect(error.message).toContain("ACP host startup timed out");
    expect(startupSignal?.aborted).toBe(true);
    expect(budget.reservedBytes).toBe(67_108_864);
    native.resolve(state.port);
    await Promise.all(observed);
    expect(state.closes).toBe(1);
    expect(state.frees).toBe(1);
    expect(budget.reservedBytes).toBe(0);
  } finally { jest.useRealTimers(); }
});

test("creation close failure remains visible and keeps the host reservation", async () => {
  const state = creationHost();
  const budget = new WasmBudget();
  const closeFailure = new Error("creation native close failed");
  state.port.close = async () => { throw closeFailure; };
  const start: StartTransport = (_env, resources, signal) => startOwnedWasmHost({
    config: "{}", resources, signal, budget,
    load: async () => ({ PeriWasmAcp: { start: async () => state.port } }),
  });
  await expect(createPeriSession({} as Env, "failed cleanup", start, () => {})).rejects.toBe(closeFailure);
  expect(state.frees).toBe(0);
  expect(budget.reservedBytes).toBe(67_108_864);
});

test("creation capacity overload returns 503 and Retry-After with the original diagnostic cause", async () => {
  const budget = new WasmBudget();
  const live = budget.acquire();
  const observed: Promise<unknown>[] = [];
  const start: StartTransport = (_env, resources, signal) => startOwnedWasmHost({
    config: "{}", resources, signal, budget,
    load: async () => { throw new Error("must not load"); },
  });
  const error = await createPeriSession({} as Env, "overloaded", start, (promise) => { observed.push(promise); })
    .catch((error) => error);
  expect(error).toBeInstanceOf(HTTPException);
  expect(error.cause.cause).toBeInstanceOf(WasmCapacityError);
  const response = errorResponse(error);
  expect(response.status).toBe(503);
  expect(response.headers.get("Retry-After")).toBe("1");
  expect(response.headers.get("Cache-Control")).toBe("no-store");
  expect((await response.json()).error).toContain("WASM isolate reservation exhausted");
  await Promise.all(observed);
  expect(budget.reservedBytes).toBe(67_108_864);
  live.releaseAfterCleanup();
});
