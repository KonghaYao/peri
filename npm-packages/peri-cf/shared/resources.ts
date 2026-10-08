export interface WasmResourceSnapshot {
  instanceId: string;
  generationId: string | null;
  phase: "loading" | "starting" | "ready" | "closing" | "closed" | "failed" | "close-unconfirmed";
  startedAt: string;
  sampledAt: string;
  memory: {
    scope: "wasm-linear-memory";
    allocatedBytes: number;
    pages: number;
    peakObservedBytes: number;
    observation: "live" | "last-observed";
    observedAt: string;
    heapUsedBytes: null;
  } | null;
  cpu: {
    supported: false;
    timeMs: null;
    utilizationPercent: null;
    reason: "worker-per-instance-cpu-unavailable";
  };
}

export interface AgentResources {
  sessionId: string;
  running: boolean;
  executionBlocked: boolean;
  instance: WasmResourceSnapshot | null;
}
