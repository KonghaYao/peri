import { z } from "zod";

export const chatIdSchema = z.guid().transform((id) => id.toLowerCase());
export const chatSchema = z.object({ id: z.string(), title: z.string(), updatedAt: z.string() });
/**
 * 列表展示的事实字段：`messageCount` 来自存储层会话摘要；运行态字段只代表
 * 当前实例驻留运行期的事实，非驻留会话为 false/0/null，不伪造。
 */
export const chatSummarySchema = chatSchema.extend({
  messageCount: z.number().int().nonnegative(),
  running: z.boolean(),
  pendingInteractionCount: z.number().int().nonnegative(),
  lastTurnStatus: z.string().nullable(),
  lastCompletedAt: z.string().nullable(),
});
export const chatListSchema = z.object({ chats: z.array(chatSummarySchema) });
export const createChatBodySchema = z.object({
  title: z.string().max(200).refine((title) => title.trim().length > 0).transform((title) => title.trim()).default("New chat"),
});
export const sendMessageBodySchema = z.object({
  content: z.string().max(32_768).refine((content) => content.trim().length > 0),
});

/** 会话命令的 wire 契约：前端 HTTP 客户端与服务端共用同一份校验。 */
export const chatCommandSchema = z.discriminatedUnion("op", [
  z.object({ op: z.literal("prompt"), content: z.string().max(32_768).refine((content) => content.trim().length > 0) }),
  z.object({ op: z.literal("cancel") }),
  z.object({ op: z.literal("interaction"), interactionId: z.string().min(1), answer: z.unknown() }),
  z.object({ op: z.literal("payload"), refId: z.string().min(1), bytes: z.number().int().nonnegative().optional() }),
]);

export type ChatCommand = z.infer<typeof chatCommandSchema>;
export const chatCommandPayloadSchema = z.object({
  id: z.string().min(1), bytes: z.number().int().nonnegative(), json: z.string(),
});
export type ChatCommandPayload = z.infer<typeof chatCommandPayloadSchema>;

/**
 * 命令失败的处置语义，也是 wire 上判断可重试性的唯一权威：
 * `rejected` 命令明确未生效；`unknown` 执行状态未知，不得自动重试。
 * 客户端另有 `not_sent`（命令未离开客户端），只存在于浏览器侧，不进入 wire。
 */
export const commandDispositionSchema = z.enum(["rejected", "unknown"]);
export const commandFailureSchema = z.object({
  ok: z.literal(false), code: z.string().min(1), message: z.string(), disposition: commandDispositionSchema,
});
export const commandSuccessSchema = z.object({ ok: z.literal(true), payload: chatCommandPayloadSchema.optional() });
export const commandResponseSchema = z.discriminatedUnion("ok", [commandSuccessSchema, commandFailureSchema]);
export type CommandDisposition = z.infer<typeof commandDispositionSchema>;
export type CommandFailureDTO = z.infer<typeof commandFailureSchema>;

export type Chat = z.infer<typeof chatSchema>;
export type ChatSummary = z.infer<typeof chatSummarySchema>;

// —— 公开展示 DTO：首屏读取与运行期投影同源，由 SDK view 物化，不额外持久化。——

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
/** AskUserQuestion 选项的 wire 值（`const`）与展示文本（`title`）；description 可选。 */
export const askUserQuestionOptionSchema = z.object({
  value: z.string().min(1), label: z.string().min(1), description: z.string().optional(),
});
/** AskUserQuestion 单题：id 是 elicitation schema 的属性键（`ask_user_question_<index>`）。 */
export const askUserQuestionSchema = z.object({
  id: z.string().min(1), header: z.string(), question: z.string().min(1),
  multiSelect: z.boolean(), options: z.array(askUserQuestionOptionSchema),
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
  /** AskUserQuestion 请求的题目；与通用 elicitation `fields` 互斥（由客户端投影注入）。 */
  questions: z.array(askUserQuestionSchema).optional(),
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

/** turn 摘要事实的唯一规则：当前 turn 与最后一条成功完成的 turn。 */
export interface TurnFacts {
  turnId: string;
  status: string;
  completedAt: string | null;
}

export function turnPresentationFacts<T extends TurnFacts>(
  turns: readonly T[], activeTurnId: string | null,
): { current: T | undefined; lastCompletedAt: string | null } {
  const current = activeTurnId
    ? turns.find((turn) => turn.turnId === activeTurnId) ?? turns.at(-1)
    : turns.at(-1);
  // 只认成功完成的 turn：运行中/失败/cancelled 的完成时间不冒充「最后完成」。
  const completed = [...turns].reverse().find((turn) => turn.status === "completed");
  return { current, lastCompletedAt: completed?.completedAt ?? null };
}

export type PublicErrorDTO = z.infer<typeof publicErrorSchema>;
export type EntryBlockDTO = z.infer<typeof entryBlockSchema>;
export type EntryDTO = z.infer<typeof entrySchema>;
export type ToolCallDTO = z.infer<typeof toolCallSchema>;
export type TurnDTO = z.infer<typeof turnSchema>;
export type InteractionDTO = z.infer<typeof interactionSchema>;
export type AskUserQuestionDTO = z.infer<typeof askUserQuestionSchema>;
export type AskUserQuestionOptionDTO = z.infer<typeof askUserQuestionOptionSchema>;
export type TaskDTO = z.infer<typeof taskSchema>;
export type PlanEntryDTO = z.infer<typeof planEntrySchema>;
export type InputQueueDTO = z.infer<typeof inputQueueSchema>;
export type SessionInfoDTO = z.infer<typeof sessionInfoSchema>;
export type ActiveTurnDTO = z.infer<typeof activeTurnSchema>;
