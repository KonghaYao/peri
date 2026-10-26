import type { SessionView } from "@peri-code/sdk/view";
import { z } from "zod";
import {
  activeTurnSchema, chatSchema, entrySchema, inputQueueSchema, interactionSchema, planEntrySchema,
  sessionInfoSchema, taskSchema, toolCallSchema, turnSchema,
  type Chat,
} from "./contracts";

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

/**
 * 纯展示适配：只复制 SDK 副本已确认的公开对象，不补造身份、时间或运行态。
 *
 * `readiness !== "ready"` 时不提供条目内容：`unsupported` 必须由 UI 显示
 * 版本不受支持，不能当成空历史；`not-ready` 只表示副本尚未收到文档对。
 */
export function materializeChatDisplayState(view: SessionView, chat: Chat): ChatDisplayState {
  const known = view.readiness === "ready";
  return chatDisplayStateSchema.parse({
    chat,
    sessionId: view.sessionId,
    bindingRevision: view.bindingRevision,
    running: known ? view.running : false,
    executionBlocked: known ? view.executionBlocked : false,
    readiness: view.readiness,
    entries: known ? view.entries : [],
    toolCalls: known ? view.toolCalls : [],
    turns: known ? view.turns : [],
    activeTurn: known ? view.activeTurn : { turnId: null, status: null },
    session: known ? view.session : {},
    tasks: known ? view.tasks : [],
    plans: { current: known ? view.plan : [], byTurn: known ? view.plansByTurn : {} },
    interactions: known ? view.interactions : [],
    pendingInteractions: known ? view.pendingInteractions : [],
    inputQueue: known ? view.inputQueue : null,
  });
}
