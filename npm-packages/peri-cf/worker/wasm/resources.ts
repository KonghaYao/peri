import type { WasmResourceSnapshot, WasmStartupStage, WasmStartupTimings } from "../../shared/resources";
import { CF_WASM_MAX_MEMORY_BYTES } from "../../shared/runtime-limits";

const STARTUP_FIELDS = {
  "module-ready": "moduleReadyMs",
  "native-ready": "nativeReadyMs",
  "acp-ready": "acpReadyMs",
} as const satisfies Record<WasmStartupStage, keyof WasmStartupTimings>;

export class WasmResources {
  readonly instanceId = crypto.randomUUID();
  private readonly startedAt = new Date().toISOString();
  private readonly startup: WasmStartupTimings = { moduleReadyMs: null, nativeReadyMs: null, acpReadyMs: null };
  private generationId: string | null = null;
  private phase: WasmResourceSnapshot["phase"] = "loading";
  private memories: WebAssembly.Memory[] = [];
  private lastMemory: WasmResourceSnapshot["memory"] = null;
  private peakObservedBytes = 0;
  private endedAt: string | null = null;

  attach(instance: WebAssembly.Instance): void {
    this.memories = [...new Set(Object.values(instance.exports)
      .filter((value): value is WebAssembly.Memory => value instanceof WebAssembly.Memory))];
    this.phase = "starting";
    const snapshot = this.snapshot();
    if ((snapshot.memory?.allocatedBytes ?? 0) > CF_WASM_MAX_MEMORY_BYTES)
      throw new Error(`WASM linear memory exceeds the ${CF_WASM_MAX_MEMORY_BYTES} byte CF host reservation`);
  }

  // 启动阶段耗时来自宿主侧的单调计时，缺失为 null，不推算也不补零。
  markStartup(stage: WasmStartupStage, elapsedMs: number): void {
    this.startup[STARTUP_FIELDS[stage]] = Math.max(0, Math.round(elapsedMs));
  }

  ready(generationId?: string): void {
    this.generationId = generationId ?? null;
    this.phase = "ready";
  }

  closing(): void { this.phase = "closing"; }

  closed(): void { this.endedAt ??= new Date().toISOString(); this.finish("closed"); }

  failed(): void { this.endedAt ??= new Date().toISOString(); this.finish("failed"); }

  closeUnconfirmed(): void {
    this.endedAt ??= new Date().toISOString();
    this.phase = "close-unconfirmed";
  }

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
      startedAt: this.startedAt, sampledAt, endedAt: this.endedAt, startup: { ...this.startup },
      memory: this.lastMemory ? { ...this.lastMemory } : null,
      cpu: {
        supported: false, timeMs: null, utilizationPercent: null,
        reason: "worker-per-instance-cpu-unavailable",
      },
    };
  }
}
