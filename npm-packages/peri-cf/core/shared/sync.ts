import { z } from "zod";
import {
  activeTurnSchema, chatSchema, entrySchema, inputQueueSchema, interactionSchema, planEntrySchema,
  sessionInfoSchema, taskSchema, toolCallSchema, turnSchema,
} from "./chat";

/**
 * 应用展示 DTO：由 SDK `ChatReplica` 的只读 `SessionView` 物化。
 *
 * 不复制运行控制状态；`running`/`executionBlocked` 是副本中的运行侧事实，
 * 不是客户端乐观推断。`readiness` 不区分「空历史」与「不支持的文档」。
 */

const chatReadinessSchema = z.enum(["not-ready", "ready", "unsupported"]);

export const chatDisplayStateSchema = z.object({
  chat: chatSchema,
  /** 副本当前绑定的 Session；尚未就绪时为 null。 */
  sessionId: z.string().nullable(),
  /** 副本当前绑定代次；缓存按 sessionId + bindingRevision 分代。 */
  bindingRevision: z.number().int().nonnegative().nullable(),
  running: z.boolean(),
  executionBlocked: z.boolean(),
  readiness: chatReadinessSchema,
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

export type ChatDisplayState = z.infer<typeof chatDisplayStateSchema>;
