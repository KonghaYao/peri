import type { SessionView } from "@peri-code/sdk/view";
import { chatDisplayStateSchema, type ChatDisplayState } from "./sync";
import type { Chat } from "./chat";

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
