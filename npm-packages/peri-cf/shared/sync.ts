import { z } from "zod";
import {
  activeTurnSchema, chatSchema, entrySchema, inputQueueSchema, interactionSchema, planEntrySchema,
  sendMessageBodySchema, sessionInfoSchema, taskSchema, toolCallSchema, turnSchema,
} from "./chat";

/** App-owned presentation DTOs. Document frames and delivery credit come from the SDK sync wire. */

/**
 * 应用控制帧走同一条 WS 连接的 **文本** 帧；SDK 文档同步帧是二进制帧（tag 0x01–0x04）。
 * 会话命令必须经这条连接下发：执行驻留在接受连接的 isolate，HTTP 没有会话亲和性。
 */
export const controlFrameIdSchema = z.string().min(1).max(64);

export const chatCommandSchema = z.discriminatedUnion("op", [
  z.object({ type: z.literal("command"), id: controlFrameIdSchema, op: z.literal("prompt"),
    content: sendMessageBodySchema.shape.content }).strict(),
  z.object({ type: z.literal("command"), id: controlFrameIdSchema, op: z.literal("cancel") }).strict(),
  z.object({ type: z.literal("command"), id: controlFrameIdSchema, op: z.literal("interaction"),
    interactionId: z.string().min(1).max(128), answer: z.unknown() }).strict(),
  z.object({ type: z.literal("command"), id: controlFrameIdSchema, op: z.literal("payload"),
    /** `bytes` 可选：带上引用记录的字节数即可断言完整性（不符回 `integrity_error`）。 */
    refId: z.string().min(1).max(256), bytes: z.number().int().nonnegative().optional() }).strict(),
]);

export const chatCommandResultSchema = z.object({ type: z.literal("command_result"), id: controlFrameIdSchema,
  ok: z.boolean(), error: z.object({ code: z.string().min(1).max(64), message: z.string().max(512) }).optional(),
  payload: z.object({ id: z.string().min(1).max(256), bytes: z.number().int().nonnegative(), json: z.string() }).optional() });

export type ChatCommand = z.infer<typeof chatCommandSchema>;
export type ChatCommandResult = z.infer<typeof chatCommandResultSchema>;

/** 文本帧解析：只有应用控制帧走文本，SDK 同步帧一律二进制。 */
export function parseControlFrame(data: unknown): ChatCommand | null {
  if (typeof data !== "string") return null;
  try { return chatCommandSchema.parse(JSON.parse(data)); }
  catch { throw new Error("Invalid control frame"); }
}

export function encodeCommandFrame(command: ChatCommand): string { return JSON.stringify(command); }

export function encodeControlFrame(frame: ChatCommandResult): string { return JSON.stringify(frame); }

/** 应用自有的运行元数据；展示身份/时间/错误由 SDK Entry 提供，不再复制。 */
export const syncMetadataSchema = z.object({
  chat: chatSchema, running: z.boolean(), executionBlocked: z.boolean(), error: z.string().max(512).optional(),
});

export const syncReadinessSchema = z.enum(["not-ready", "ready", "unsupported"]);
export const syncStateSchema = z.object({
  chat: chatSchema,
  running: z.boolean(),
  executionBlocked: z.boolean(),
  error: z.string().max(512).optional(),
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
