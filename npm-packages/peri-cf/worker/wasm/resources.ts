import type { WasmResourceSnapshot } from "../../shared/resources";
import { CF_WASM_MAX_MEMORY_BYTES } from "../../shared/runtime-limits";

export class WasmResources {
  readonly instanceId = crypto.randomUUID();
  private readonly startedAt = new Date().toISOString();
  private generationId: string | null = null;
  private phase: WasmResourceSnapshot["phase"] = "loading";
  private memories: WebAssembly.Memory[] = [];
  private lastMemory: WasmResourceSnapshot["memory"] = null;
  private peakObservedBytes = 0;

  attach(instance: WebAssembly.Instance): void {
    this.memories = [...new Set(Object.values(instance.exports)
      .filter((value): value is WebAssembly.Memory => value instanceof WebAssembly.Memory))];
    this.phase = "starting";
    const snapshot = this.snapshot();
    if ((snapshot.memory?.allocatedBytes ?? 0) > CF_WASM_MAX_MEMORY_BYTES)
      throw new Error(`WASM linear memory exceeds the ${CF_WASM_MAX_MEMORY_BYTES} byte CF host reservation`);
  }

  ready(generationId?: string): void {
    this.generationId = generationId ?? null;
    this.phase = "ready";
  }

  closing(): void { this.phase = "closing"; }

  closed(): void { this.finish("closed"); }

  failed(): void { this.finish("failed"); }

  closeUnconfirmed(): void { this.phase = "close-unconfirmed"; }

  private finish(phase: "closed" | "failed"): void {
    this.snapshot();
    this.memories = [];
    if (this.lastMemory) this.lastMemory.observation = "last-observed";
    this.phase = phase;
  }

  snapshot(): WasmResourceSnapshot {
    const sampledAt = new Date().toISOString();
    if (this.memories.length) {
      const allocatedBytes = this.memories.reduce((total, memory) => total + memory.buffer.byteLength, 0);
      this.peakObservedBytes = Math.max(this.peakObservedBytes, allocatedBytes);
      this.lastMemory = {
        scope: "wasm-linear-memory", allocatedBytes, pages: allocatedBytes / 65_536,
        peakObservedBytes: this.peakObservedBytes, observation: "live", observedAt: sampledAt,
        heapUsedBytes: null,
      };
    }
    return {
      instanceId: this.instanceId, generationId: this.generationId, phase: this.phase,
      startedAt: this.startedAt, sampledAt, memory: this.lastMemory ? { ...this.lastMemory } : null,
      cpu: {
        supported: false, timeMs: null, utilizationPercent: null,
        reason: "worker-per-instance-cpu-unavailable",
      },
    };
  }
}
