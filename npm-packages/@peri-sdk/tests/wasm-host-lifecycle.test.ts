import { describe, expect, test } from "bun:test";
import type { WasmAcpTransport } from "../src/transport/wasm-transport";
import { DeadlineError } from "../src/util/deadline";
import {
  PeriWasmHostLease, PeriWasmHostManager, type PeriWasmHostStarter,
} from "../src/wasm/host-lifecycle";

let generation = 0;

function fakeTransport(close: () => Promise<void> = async () => {}): WasmAcpTransport {
  generation += 1;
  return { generationId: `generation-${generation}`, close } as unknown as WasmAcpTransport;
}

describe("PeriWasmHostLease", () => {
  test("start returns a live lease and close is confirmed and idempotent", async () => {
    let closes = 0;
    const lease = await PeriWasmHostLease.start(async () => fakeTransport(async () => { closes += 1; }));
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
    const lease = await PeriWasmHostLease.start(async () => fakeTransport(async () => { throw error; }));
    const report = await lease.close();
    expect(report.confirmed).toBe(false);
    expect(report.error).toBe(error);
    expect(lease.state).toBe("close-unconfirmed");
  });

  test("startup timeout aborts the starter signal", async () => {
    let signal: AbortSignal | undefined;
    const starter: PeriWasmHostStarter = ({ signal: startupSignal }) => {
      signal = startupSignal;
      return new Promise<WasmAcpTransport>(() => {});
    };
    await expect(PeriWasmHostLease.start(starter, { startupTimeoutMs: 5 }))
      .rejects.toBeInstanceOf(DeadlineError);
    expect(signal?.aborted).toBe(true);
  });

  test("a late startup result is closed and reported", async () => {
    let resolveStart!: (transport: WasmAcpTransport) => void;
    let closes = 0;
    const outcomes: { confirmed: boolean }[] = [];
    const starting = new Promise<WasmAcpTransport>((resolve) => { resolveStart = resolve; });
    await expect(PeriWasmHostLease.start(() => starting, {
      startupTimeoutMs: 5,
      onLateCleanup: (outcome) => outcomes.push(outcome),
    })).rejects.toBeInstanceOf(DeadlineError);
    resolveStart(fakeTransport(async () => { closes += 1; }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(closes).toBe(1);
    expect(outcomes).toEqual([{ confirmed: true }]);
  });
});

describe("PeriWasmHostManager", () => {
  test("tracks acquired leases and closeAll closes them", async () => {
    let closes = 0;
    const manager = new PeriWasmHostManager(async () => fakeTransport(async () => { closes += 1; }));
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
    const manager = new PeriWasmHostManager(({ signal }) => new Promise<WasmAcpTransport>((_, reject) => {
      startupSignal = signal;
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
    const manager = new PeriWasmHostManager(async () => fakeTransport(async () => {
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
});
