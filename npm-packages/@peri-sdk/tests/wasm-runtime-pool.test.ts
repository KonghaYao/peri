import { describe, expect, test } from "bun:test";
import type { WasmAcpTransport } from "../src/wasm/transport";
import { DeadlineError } from "../src/util/deadline";
import {
  WasmRuntimeLease, WasmRuntimePool, type WasmRuntimeStarter,
} from "../src/wasm/runtime-pool";
import {
  WasmRuntimeStartupError, type WasmRuntimeCleanupOutcome,
} from "../src/wasm/runtime";

let generation = 0;

function fakeTransport(close: () => Promise<void> = async () => {}): WasmAcpTransport {
  generation += 1;
  return { generationId: `generation-${generation}`, close } as unknown as WasmAcpTransport;
}

describe("WasmRuntimeLease", () => {
  test("start returns a live lease and close is confirmed and idempotent", async () => {
    let closes = 0;
    const lease = await WasmRuntimeLease.start(async () => fakeTransport(async () => { closes += 1; }));
    expect(lease.state).toBe("live");
    expect(lease.generationId).toMatch(/^generation-/);
    const first = await lease.close();
    const second = await lease.close();
    expect(first).toEqual(second);
    expect(closes).toBe(1);
    expect(first.confirmed).toBe(true);
    expect(lease.state).toBe("closed");
  });

  test("close failure is reported as close-unconfirmed without throwing", async () => {
    const error = new Error("native close failed");
    const lease = await WasmRuntimeLease.start(async () => fakeTransport(async () => { throw error; }));
    const report = await lease.close();
    expect(report.confirmed).toBe(false);
    expect(report.error).toBe(error);
    expect(lease.state).toBe("close-unconfirmed");
  });

  test("startup timeout aborts the starter signal", async () => {
    let signal: AbortSignal | undefined;
    const starter: WasmRuntimeStarter = ({ signal: startupSignal }) => {
      signal = startupSignal;
      return new Promise<WasmAcpTransport>(() => {});
    };
    await expect(WasmRuntimeLease.start(starter, { startupTimeoutMs: 5 }))
      .rejects.toBeInstanceOf(DeadlineError);
    expect(signal?.aborted).toBe(true);
  });

  test("a late startup result is closed and reported", async () => {
    let resolveStart!: (transport: WasmAcpTransport) => void;
    let closes = 0;
    const outcomes: { confirmed: boolean }[] = [];
    const starting = new Promise<WasmAcpTransport>((resolve) => { resolveStart = resolve; });
    await expect(WasmRuntimeLease.start(() => starting, {
      startupTimeoutMs: 5,
      onLateCleanup: (outcome) => outcomes.push(outcome),
    })).rejects.toBeInstanceOf(DeadlineError);
    resolveStart(fakeTransport(async () => { closes += 1; }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(closes).toBe(1);
    expect(outcomes).toEqual([{ confirmed: true }]);
  });

  test("a late startup rejection settles its cleanup evidence", async () => {
    let rejectStart!: (error: unknown) => void;
    const outcomes: WasmRuntimeCleanupOutcome[] = [];
    const starting = new Promise<WasmAcpTransport>((_, reject) => { rejectStart = reject; });
    await expect(WasmRuntimeLease.start(() => starting, {
      startupTimeoutMs: 5,
      onLateCleanup: (outcome) => outcomes.push(outcome),
    })).rejects.toBeInstanceOf(DeadlineError);
    const cleanupError = new Error("native exit unconfirmed");
    rejectStart(new WasmRuntimeStartupError(
      new Error("startup failed"),
      Promise.resolve({ confirmed: false, error: cleanupError }),
    ));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(outcomes).toEqual([{ confirmed: false, error: cleanupError }]);
  });
});

describe("WasmRuntimePool", () => {
  test("acquire passes per-chat startup config to the starter", async () => {
    const starts: Array<{
      configJson?: string;
      env?: Readonly<Record<string, string>>;
      diagnostic?: unknown;
    }> = [];
    const diagnostic = (): void => {};
    const manager = new WasmRuntimePool(async (options) => {
      starts.push({
        configJson: options.configJson,
        env: options.env,
        diagnostic: options.onDiagnostic,
      });
      return fakeTransport();
    });

    const lease = await manager.acquire({
      configJson: '{"cwd":"/chat-a"}',
      env: { FLAG: "1" },
      onDiagnostic: diagnostic,
    });
    expect(starts).toEqual([
      {
        configJson: '{"cwd":"/chat-a"}',
        env: { FLAG: "1" },
        diagnostic,
      },
    ]);
    await manager.release(lease);
  });

  test("tracks acquired leases and closeAll closes them", async () => {
    let closes = 0;
    const manager = new WasmRuntimePool(async () => fakeTransport(async () => { closes += 1; }));
    const first = await manager.acquire();
    const second = await manager.acquire();
    expect(manager.size).toBe(2);
    const reports = await manager.closeAll();
    expect(reports.map((report) => report.confirmed)).toEqual([true, true]);
    expect(closes).toBe(2);
    expect(manager.size).toBe(0);
    expect(first.state).toBe("closed");
    expect(second.state).toBe("closed");
  });

  test("closeAll aborts an in-flight startup and waits for its rejection", async () => {
    let startupSignal: AbortSignal | undefined;
    const manager = new WasmRuntimePool(({ signal }) => new Promise<WasmAcpTransport>((_, reject) => {
      startupSignal = signal;
      if (signal.aborted) {
        reject(signal.reason);
        return;
      }
      signal.addEventListener("abort", () => reject(signal.reason));
    }), { startupTimeoutMs: 1_000 });
    const acquiring = manager.acquire();
    const closing = manager.closeAll();
    await expect(acquiring).rejects.toBeDefined();
    await closing;
    expect(startupSignal?.aborted).toBe(true);
    expect(manager.size).toBe(0);
  });

  test("release drops a confirmed lease and keeps an unconfirmed one", async () => {
    let shouldFail = false;
    const manager = new WasmRuntimePool(async () => fakeTransport(async () => {
      if (shouldFail) throw new Error("close failed");
    }));
    const confirmed = await manager.acquire();
    expect((await manager.release(confirmed)).confirmed).toBe(true);
    expect(manager.size).toBe(0);

    shouldFail = true;
    const unconfirmed = await manager.acquire();
    expect((await manager.release(unconfirmed)).confirmed).toBe(false);
    expect(manager.unconfirmed).toBe(true);
    expect(manager.size).toBe(1);
  });

  test("waits for late cleanup after a failed startup and remembers unconfirmed ownership", async () => {
    let resolveCleanup!: (outcome: WasmRuntimeCleanupOutcome) => void;
    const cleanup = new Promise<WasmRuntimeCleanupOutcome>((resolve) => { resolveCleanup = resolve; });
    const manager = new WasmRuntimePool(async () => {
      throw new WasmRuntimeStartupError(new Error("startup failed"), cleanup);
    });
    await expect(manager.acquire()).rejects.toBeInstanceOf(WasmRuntimeStartupError);
    expect(manager.unconfirmed).toBe(false);

    const closing = manager.closeAll();
    let closed = false;
    void closing.then(() => { closed = true; });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(closed).toBe(false);

    resolveCleanup({ confirmed: false, error: new Error("native exit unconfirmed") });
    await closing;
    expect(manager.unconfirmed).toBe(true);
  });

  test("closeAll bounds a late cleanup that never settles", async () => {
    const manager = new WasmRuntimePool(
      () => new Promise<WasmAcpTransport>(() => {}),
      { startupTimeoutMs: 5, closeTimeoutMs: 20 },
    );
    const acquiring = manager.acquire();
    await new Promise((resolve) => setTimeout(resolve, 0));
    const closing = manager.closeAll();
    await expect(acquiring).rejects.toBeDefined();
    await closing;
    expect(manager.unconfirmed).toBe(true);
  });
});
