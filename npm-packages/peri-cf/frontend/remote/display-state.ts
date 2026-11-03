import type { SessionView } from "@peri-code/sdk/view";
import { z } from "zod";
import {
  activeTurnSchema, chatSchema, entrySchema, inputQueueSchema, interactionSchema, planEntrySchema,
  sessionInfoSchema, taskSchema, toolCallSchema, turnSchema,
  type Chat,
} from "./contracts";
import { logError } from "./logging";

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

/** 单项校验：坏条目被丢弃，同批其余条目仍然可见，不让一个字段冻结整份展示态。 */
function keepValid<T>(schema: z.ZodType<T>, values: unknown): T[] {
  if (!Array.isArray(values)) return [];
  return values.flatMap((value) => {
    const parsed = schema.safeParse(value);
    return parsed.success ? [parsed.data] : [];
  });
}

function keepRecord<T>(schema: z.ZodType<T>, values: unknown): Record<string, T[]> {
  if (values === null || typeof values !== "object" || Array.isArray(values)) return {};
  return Object.fromEntries(
    Object.entries(values as Record<string, unknown>)
      .map(([key, value]) => [key, keepValid(schema, value)] as const)
      // 空数组没有展示价值，也避免把未知键带进视图。
      .filter(([, list]) => list.length > 0),
  );
}

/** 每次构造新的空值，避免不同展示态之间共享可变对象。 */
const emptyActiveTurn = (): ChatDisplayState["activeTurn"] => ({ turnId: null, status: null });
const emptySession = (): ChatDisplayState["session"] => ({});

/** 单字段校验：不通过时返回 undefined，由调用方给出空/中性值。 */
function parseField<T>(schema: z.ZodType<T>, value: unknown): T | undefined {
  const parsed = schema.safeParse(value);
  return parsed.success ? parsed.data : undefined;
}

/**
 * 字段级回退：一次逐字段兜底构造展示 DTO。
 *
 * 正常路径与 salvage 路径共用这一份映射；`readiness !== "ready"` 时不提供条目内容，
 * 只退到「更少事实」，不猜测运行态。
 */
function buildDisplayState(
  view: SessionView,
  chat: Chat,
  known: boolean,
): ChatDisplayState {
  return {
    chat,
    sessionId: view.sessionId,
    bindingRevision: view.bindingRevision,
    running: known && view.running === true,
    executionBlocked: known && view.executionBlocked === true,
    readiness: view.readiness,
    entries: known ? keepValid(entrySchema, view.entries) : [],
    toolCalls: known ? keepValid(toolCallSchema, view.toolCalls) : [],
    turns: known ? keepValid(turnSchema, view.turns) : [],
    activeTurn: parseField(activeTurnSchema, known ? view.activeTurn : emptyActiveTurn())
      ?? emptyActiveTurn(),
    session: parseField(sessionInfoSchema, known ? view.session : {}) ?? emptySession(),
    tasks: known ? keepValid(taskSchema, view.tasks) : [],
    plans: {
      current: known ? keepValid(planEntrySchema, view.plan) : [],
      byTurn: known ? keepRecord(planEntrySchema, view.plansByTurn) : {},
    },
    interactions: known ? keepValid(interactionSchema, view.interactions) : [],
    pendingInteractions: known ? keepValid(interactionSchema, view.pendingInteractions) : [],
    inputQueue: parseField(inputQueueSchema, known ? view.inputQueue : null) ?? null,
  };
}

/**
 * 纯展示适配：只复制 SDK 副本已确认的公开对象，不补造身份、时间或运行态。
 *
 * `readiness !== "ready"` 时不提供条目内容：`unsupported` 必须由 UI 显示
 * 版本不受支持，不能当成空历史；`not-ready` 只表示副本尚未收到文档对。
 * 校验失败只降级为可展示的子集，绝不让解析异常穿透副本通知。
 *
 * 字段映射只在 `buildDisplayState` 维护一份：整体 schema 通过就采用其规范化结果，
 * 失败则原样返回同一份逐字段兜底构造，并留下日志用于发现 schema 与构造的漂移。
 */
export function materializeChatDisplayState(view: SessionView, chat: Chat): ChatDisplayState {
  const known = view.readiness === "ready";
  const state = buildDisplayState(view, chat, known);
  const parsed = chatDisplayStateSchema.safeParse(state);
  if (parsed.success) return parsed.data;
  logError("Chat display state failed schema validation; keeping valid facts", parsed.error, {
    sessionId: view.sessionId,
    bindingRevision: view.bindingRevision,
  });
  return state;
}
