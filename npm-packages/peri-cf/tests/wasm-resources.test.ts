import { describe, expect, test } from "bun:test";
import { WasmResources } from "../worker/wasm/resources";
import { memoryInstance } from "./helpers/wasm";

describe("per-instance WASM resource observations", () => {
  test("startup has no fabricated memory or CPU measurements", () => {
    const snapshot = new WasmResources().snapshot();
    expect(snapshot.phase).toBe("loading");
    expect(snapshot.memory).toBeNull();
    expect(snapshot.generationId).toBeNull();
    expect(snapshot.cpu).toEqual({ supported: false, timeMs: null,
      utilizationPercent: null, reason: "worker-per-instance-cpu-unavailable" });
  });

  test("reads actual exported memory including growth and preserves independent snapshots", () => {
    const resources = new WasmResources();
    const instance = memoryInstance();
    resources.attach(instance);
    const before = resources.snapshot();
    expect(before.phase).toBe("starting");
    expect(before.memory?.allocatedBytes).toBe(65_536);
    (instance.exports.m as WebAssembly.Memory).grow(2);
    resources.ready("actual-transport-generation");
    const after = resources.snapshot();
    expect(after.generationId).toBe("actual-transport-generation");
    expect(after.memory).toMatchObject({ allocatedBytes: 196_608, pages: 3,
      peakObservedBytes: 196_608, observation: "live", scope: "wasm-linear-memory", heapUsedBytes: null });
    expect(before.memory?.allocatedBytes).toBe(65_536);
    expect(after.instanceId).toBe(before.instanceId);
  });

  test("closed instances retain a final observation rather than claiming live memory", () => {
    const resources = new WasmResources();
    const instance = memoryInstance();
    resources.attach(instance);
    resources.closing();
    expect(resources.snapshot().phase).toBe("closing");
    resources.closed();
    (instance.exports.m as WebAssembly.Memory).grow(1);
    const snapshot = resources.snapshot();
    expect(snapshot.phase).toBe("closed");
    expect(snapshot.memory?.observation).toBe("last-observed");
    expect(snapshot.memory?.allocatedBytes).toBe(65_536);
  });

  test("records host startup stage timings and the observed terminal time", () => {
    const resources = new WasmResources();
    expect(resources.snapshot()).toMatchObject({
      endedAt: null,
      startup: { moduleReadyMs: null, nativeReadyMs: null, acpReadyMs: null },
    });
    resources.markStartup("module-ready", 3.4);
    resources.markStartup("native-ready", 71.6);
    resources.markStartup("acp-ready", -5);
    resources.attach(memoryInstance());
    resources.ready("generation-1");
    expect(resources.snapshot()).toMatchObject({
      phase: "ready", endedAt: null,
      startup: { moduleReadyMs: 3, nativeReadyMs: 72, acpReadyMs: 0 },
    });
    const closedAt = () => resources.snapshot().endedAt;
    resources.closing();
    expect(closedAt()).toBeNull();
    resources.closed();
    const endedAt = closedAt();
    expect(endedAt).not.toBeNull();
    resources.closeUnconfirmed();
    expect(closedAt()).toBe(endedAt);
  });

  test("failed startup and unconfirmed close are distinct and instances are isolated", () => {
    const failed = new WasmResources();
    failed.failed();
    expect(failed.snapshot()).toMatchObject({ phase: "failed", memory: null });
    const unconfirmed = new WasmResources();
    unconfirmed.attach(memoryInstance());
    unconfirmed.closeUnconfirmed();
    expect(unconfirmed.snapshot()).toMatchObject({ phase: "close-unconfirmed", memory: { observation: "live" } });
    expect(unconfirmed.snapshot().instanceId).not.toBe(failed.snapshot().instanceId);
  });

  test("an oversized initial WASM memory is rejected without fabricating a smaller observation", () => {
    const oversized = new WebAssembly.Instance(new WebAssembly.Module(new Uint8Array([
      0, 97, 115, 109, 1, 0, 0, 0,
      5, 6, 1, 1, 129, 8, 130, 8,
      7, 5, 1, 1, 109, 2, 0,
    ])));
    const resources = new WasmResources();
    expect(() => resources.attach(oversized)).toThrow("67108864 byte CF host reservation");
    expect(resources.snapshot().memory?.allocatedBytes).toBe(67_174_400);
    resources.failed();
    expect(resources.snapshot()).toMatchObject({ phase: "failed", memory: { observation: "last-observed" } });
  });
});
