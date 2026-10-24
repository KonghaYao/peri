import { z } from "zod";
import {
  activeTurnSchema, chatSchema, entrySchema, inputQueueSchema, interactionSchema, planEntrySchema,
  sessionInfoSchema, taskSchema, toolCallSchema, turnSchema,
} from "./chat";

/** App-owned presentation DTOs（会话投影的读取形态；命令通道见 `core/agent/session/runtime.ts`）。 */

/** 应用自有的运行元数据；展示身份/时间/错误由 SDK Entry 提供，不再复制。 */
export const syncMetadataSchema = z.object({
  chat: chatSchema, running: z.boolean(), executionBlocked: z.boolean(), error: z.string().max(512).optional(),
  /** 投影发布序号：每次 publish 递增，作为未来增量协议的门闩。 */
  revision: z.number().int().nonnegative().default(0),
  /** 投影实例标识：运行期重建后变化，用于识别增量基线失效。 */
  projectionEpoch: z.string().min(1).optional(),
});

export const syncReadinessSchema = z.enum(["not-ready", "ready", "unsupported"]);
export const syncStateSchema = z.object({
  chat: chatSchema,
  running: z.boolean(),
  executionBlocked: z.boolean(),
  error: z.string().max(512).optional(),
  revision: z.number().int().nonnegative().default(0),
  projectionEpoch: z.string().min(1).optional(),
  readiness: syncReadinessSchema,
  entries: z.array(entrySchema),
  toolCalls: z.array(toolCallSchema),
  turns: z.array(turnSchema),
  activeTurn: activeTurnSchema,
  session: sessionInfoSchema,
  tasks: z.array(taskSchema),
  plans: z.object({ current: z.array(planEntrySchema), byTurn: z.record(z.string(), z.array(planEntrySchema)) }),
  interactions: z.array(interactionSchema),
  pendingInteractions: z.array(interactionSchema),
  inputQueue: inputQueueSchema.nullable(),
});
export type SyncMetadata = z.infer<typeof syncMetadataSchema>;
export type SyncState = z.infer<typeof syncStateSchema>;
export type SyncReadiness = z.infer<typeof syncReadinessSchema>;
