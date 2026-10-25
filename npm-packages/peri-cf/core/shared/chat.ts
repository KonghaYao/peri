import { z } from "zod";

/** 会话目录身份：公开聊天 ID 是真实 ACP Session ID；应用不再用标题/时间之外的运行态派生它。 */
export const chatIdSchema = z.guid().transform((id) => id.toLowerCase());

/** 展示身份：`id` 与目录会话 ID 一致，标题/时间只来自目录或已绑定的副本。 */
export const chatSchema = z.object({
  id: z.string().min(1),
  title: z.string(),
  updatedAt: z.string(),
});

/**
 * 目录条目：仅服务端存储可提供的会话事实。
 *
 * 运行态不在列表里伪造；当前选中会话的运行/交互事实只能来自 Yjs 副本。
 */
export const chatSummarySchema = chatSchema.extend({
  sessionId: z.string().min(1),
  title: z.string().nullable(),
  cwd: z.string(),
  messageCount: z.number().int().nonnegative(),
  createdAt: z.string(),
});

export const chatDirectoryResponseSchema = z.object({
  chats: z.array(chatSummarySchema),
  available: z.boolean(),
  error: z.string().optional(),
});

/** `POST /api/chats` 只登记句柄；`sessionId:null` + `bindingRevision:0` 表示尚未绑定。 */
export const chatHandleSchema = z.object({
  id: z.string().min(1),
  sessionId: z.string().min(1).nullable(),
  bindingRevision: z.number().int().nonnegative(),
});

export const createChatRequestSchema = z.object({
  path: z.string().min(1).optional(),
});

/** Action 封套里的目标绑定；空句柄固定 `{sessionId:null,bindingRevision:0}`。 */
export const actionTargetSchema = z.object({
  sessionId: z.string().min(1).nullable(),
  bindingRevision: z.number().int().nonnegative(),
});

/** 202 回执：send 额外带 inputId；控制/历史动作带各自的纯数据结果。 */
export const chatActionSuccessSchema = z.object({
  ok: z.literal(true),
  requestId: z.string().min(1),
  chatId: z.string().min(1),
  sessionId: z.string(),
  bindingRevision: z.number().int().nonnegative(),
  inputId: z.string().min(1).optional(),
  accepted: z.boolean().optional(),
  status: z.string().min(1).optional(),
  applied: z.number().int().nonnegative().optional(),
  nextCursor: z.string().nullable().optional(),
});

/**
 * HTTP 错误封套：`disposition` 是唯一可重试性权威。
 *
 * `not_sent` 只由客户端在请求未提交时产生；服务端只返回 `rejected`/`unknown`。
 */
export const chatErrorResponseSchema = z.object({
  ok: z.literal(false),
  code: z.string().min(1),
  message: z.string(),
  disposition: z.enum(["rejected", "unknown", "not_sent"]),
  requestId: z.string().optional(),
  chatId: z.string().optional(),
  createdSessionId: z.string().optional(),
  stage: z.string().optional(),
  incomplete: z.boolean().optional(),
  errors: z.array(z.string()).optional(),
});

export const payloadResponseSchema = z.object({
  ok: z.literal(true),
  payload: z.object({
    id: z.string().min(1),
    bytes: z.number().int().nonnegative(),
    json: z.string(),
  }),
});

export const payloadErrorResponseSchema = z.object({
  ok: z.literal(false),
  code: z.string().min(1),
  message: z.string(),
});

// —— 公开展示 DTO：由 SDK SessionView 物化，不额外持久化。——

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
/** AskUserQuestion 选项的 wire 值（`value`）与展示文本（`label`）；description 可选。 */
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
  variant: z.literal("ask_user").optional(),
  options: z.array(z.object({ optionId: z.string(), name: z.string() })).optional(),
  fields: z.array(z.object({
    key: z.string(), title: z.string(), description: z.string(),
    type: z.enum(["string", "number", "integer", "boolean"]), required: z.boolean(),
    enum: z.array(z.union([z.string(), z.number(), z.boolean()])).optional(),
  })).optional(),
  /** AskUserQuestion 请求的题目；与通用 elicitation `fields` 互斥（来自 SDK view）。 */
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

export type Chat = z.infer<typeof chatSchema>;
export type ChatSummary = z.infer<typeof chatSummarySchema>;
export type ChatHandleDTO = z.infer<typeof chatHandleSchema>;
export type ActionTargetDTO = z.infer<typeof actionTargetSchema>;
export type ChatActionSuccessDTO = z.infer<typeof chatActionSuccessSchema>;
export type ChatErrorResponseDTO = z.infer<typeof chatErrorResponseSchema>;
export type PayloadResponseDTO = z.infer<typeof payloadResponseSchema>;
