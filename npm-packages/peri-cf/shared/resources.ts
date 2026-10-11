import { z } from "zod";

export const wasmInstancePhases = ["loading", "starting", "ready", "closing", "closed", "failed", "close-unconfirmed"] as const;

export const wasmStartupStages = ["module-ready", "native-ready", "acp-ready"] as const;
export type WasmStartupStage = (typeof wasmStartupStages)[number];

export const wasmStartupTimingsSchema = z.object({
  moduleReadyMs: z.number().nullable(),
  nativeReadyMs: z.number().nullable(),
  acpReadyMs: z.number().nullable(),
});
export type WasmStartupTimings = z.infer<typeof wasmStartupTimingsSchema>;

export const wasmResourceSnapshotSchema = z.object({
  instanceId: z.string(),
  generationId: z.string().nullable(),
  phase: z.enum(wasmInstancePhases),
  startedAt: z.string(),
  sampledAt: z.string(),
  endedAt: z.string().nullable(),
  startup: wasmStartupTimingsSchema,
  memory: z.object({
    scope: z.literal("wasm-linear-memory"),
    allocatedBytes: z.number(),
    pages: z.number(),
    peakObservedBytes: z.number(),
    observation: z.enum(["live", "last-observed"]),
    observedAt: z.string(),
    heapUsedBytes: z.null(),
  }).nullable(),
  cpu: z.object({
    supported: z.literal(false),
    timeMs: z.null(),
    utilizationPercent: z.null(),
    reason: z.literal("worker-per-instance-cpu-unavailable"),
  }),
});
export type WasmResourceSnapshot = z.infer<typeof wasmResourceSnapshotSchema>;

export const agentResourcesSchema = z.object({
  sessionId: z.string(),
  running: z.boolean(),
  executionBlocked: z.boolean(),
  instance: wasmResourceSnapshotSchema.nullable(),
});
export type AgentResources = z.infer<typeof agentResourcesSchema>;
