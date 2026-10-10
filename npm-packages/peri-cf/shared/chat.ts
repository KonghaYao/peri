import { z } from "zod";

export const messageStatuses = ["running", "completed", "cancelled", "error"] as const;
export const chatIdSchema = z.guid().transform((id) => id.toLowerCase());
export const chatSchema = z.object({ id: z.string(), title: z.string(), updatedAt: z.string() });
export const chatListSchema = z.object({ chats: z.array(chatSchema) });
export const createChatBodySchema = z.object({
  title: z.string().max(200).refine((title) => title.trim().length > 0).transform((title) => title.trim()).default("New chat"),
});
export const sendMessageBodySchema = z.object({
  content: z.string().max(32_768).refine((content) => content.trim().length > 0),
});

/**
 * 旧 DO 记录的持久行；只服务应用重启与 P3 checkpoint 迁移输入，不再是公开 DTO。
 * 展示身份/时间/错误以 SDK Entry 为权威。
 */
export const persistedMessageSchema = z.object({
  id: z.string(), role: z.enum(["user", "assistant"]), content: z.string(), createdAt: z.string(),
  status: z.enum(messageStatuses), error: z.string().optional(),
});
export type Chat = z.infer<typeof chatSchema>;
export type PersistedMessage = z.infer<typeof persistedMessageSchema>;

// —— 公开展示 DTO：HTTP 首屏与 WS 实时同源，由 SDK view 物化，不额外持久化。——

export const publicErrorSchema = z.object({ code: z.string().min(1), message: z.string() });
export const toolPayloadRefSchema = z.object({
  id: z.string().min(1), bytes: z.number().int().nonnegative(), preview: z.string(),
});
export const toolCallSchema = z.object({
  toolCallId: z.string().min(1), turnId: z.string().nullable(), entryId: z.string().optional(),
  name: z.string(), status: z.string().min(1), kind: z.string().optional(),
  permissionId: z.string().optional(),
  createdAt: z.string().nullable(), completedAt: z.string().nullable(),
  error: publicErrorSchema.nullable(),
  arguments: z.unknown().optional(), result: z.unknown().optional(),
  argumentsRef: toolPayloadRefSchema.optional(), resultRef: toolPayloadRefSchema.optional(),
});
export const entryBlockSchema = z.discriminatedUnion("type", [
  z.object({ blockId: z.string().min(1), type: z.literal("text"), text: z.string() }),
  z.object({
    blockId: z.string().min(1), type: z.literal("reasoning"), text: z.string(),
    visibility: z.enum(["summary", "hidden"]).optional(),
  }),
  z.object({
    blockId: z.string().min(1), type: z.literal("tool_call"), toolCallId: z.string().min(1),
    tool: toolCallSchema.nullable(), unavailable: z.string().optional(),
  }),
  z.object({ blockId: z.string().min(1), type: z.literal("unknown"), reason: z.string().min(1) }),
]);
export const entrySchema = z.object({
  entryId: z.string().min(1), id: z.string().min(1), kind: z.enum(["message", "tool", "system"]),
  turnId: z.string().min(1), role: z.enum(["user", "assistant"]), status: z.string().min(1),
  messageId: z.string().optional(),
  createdAt: z.string().nullable(), createdAtSource: z.string(), completedAt: z.string().nullable(),
  error: publicErrorSchema.nullable(), blocks: z.array(entryBlockSchema),
  tokenUsage: z.record(z.string(), z.unknown()).optional(),
});
export const turnSchema = z.object({
  turnId: z.string().min(1), status: z.string().min(1), requestId: z.string().optional(),
  startedAt: z.string().nullable(), completedAt: z.string().nullable(),
  error: publicErrorSchema.nullable(), stopReason: z.string().optional(),
  interactionStatus: z.string().min(1),
});
export const planEntrySchema = z.object({ content: z.string(), status: z.string(), activeForm: z.string().optional() });
export const taskSchema = z.object({
  taskId: z.string().min(1), kind: z.string(), title: z.string(), status: z.string().min(1),
  summary: z.string().optional(), isBackground: z.boolean().optional(), taskSubtype: z.string().optional(),
  startedAt: z.string().optional(), completedAt: z.string().optional(), updatedAt: z.string().optional(),
  durationMs: z.number().optional(),
});
export const interactionSchema = z.object({
  id: z.string().min(1), kind: z.enum(["permission", "elicitation"]), sessionId: z.string().min(1),
  turnId: z.string().optional(), toolCallId: z.string().optional(),
  status: z.enum(["pending", "resolved", "expired"]),
  createdAt: z.string().nullable(), resolvedAt: z.string().nullable(), outcome: z.string().optional(),
  title: z.string().optional(), message: z.string().optional(), formUnsupported: z.string().optional(),
  options: z.array(z.object({ optionId: z.string(), name: z.string() })).optional(),
  fields: z.array(z.object({
    key: z.string(), title: z.string(), description: z.string(),
    type: z.enum(["string", "number", "integer", "boolean"]), required: z.boolean(),
    enum: z.array(z.union([z.string(), z.number(), z.boolean()])).optional(),
  })).optional(),
});
export const inputQueueSchema = z.object({
  generation: z.string().min(1), revision: z.number(),
  activeRequestId: z.string().optional(),
  items: z.array(z.object({ inputId: z.string().min(1), state: z.string().min(1) })),
});
export const sessionInfoSchema = z.object({
  title: z.string().optional(), model: z.string().optional(), mode: z.string().optional(),
  updatedAt: z.string().optional(), activeRequestId: z.string().optional(),
  activeTurnInteractionStatus: z.string().optional(), compactionStatus: z.string().optional(),
  suspendedTurnId: z.string().optional(), legacyIncomplete: z.boolean().optional(),
  rewindRevision: z.number().optional(),
  modeState: z.record(z.string(), z.unknown()).optional(),
  modelState: z.record(z.string(), z.unknown()).optional(),
  goal: z.record(z.string(), z.unknown()).optional(),
  stateSnapshotMeta: z.record(z.string(), z.unknown()).optional(),
  compaction: z.record(z.string(), z.unknown()).optional(),
  configOptions: z.array(z.record(z.string(), z.unknown())).optional(),
  availableCommands: z.array(z.record(z.string(), z.unknown())).optional(),
});
export const activeTurnSchema = z.object({ turnId: z.string().nullable(), status: z.string().nullable() });

export type PublicErrorDTO = z.infer<typeof publicErrorSchema>;
export type EntryBlockDTO = z.infer<typeof entryBlockSchema>;
export type EntryDTO = z.infer<typeof entrySchema>;
export type ToolCallDTO = z.infer<typeof toolCallSchema>;
export type TurnDTO = z.infer<typeof turnSchema>;
export type InteractionDTO = z.infer<typeof interactionSchema>;
export type TaskDTO = z.infer<typeof taskSchema>;
export type PlanEntryDTO = z.infer<typeof planEntrySchema>;
export type InputQueueDTO = z.infer<typeof inputQueueSchema>;
export type SessionInfoDTO = z.infer<typeof sessionInfoSchema>;
export type ActiveTurnDTO = z.infer<typeof activeTurnSchema>;
