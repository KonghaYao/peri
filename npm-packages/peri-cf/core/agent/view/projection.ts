import { SessionDocs, type InteractionCallbacks, type JsonRpcNotification } from "@peri-code/sdk/portable";
import { SessionViewStore } from "@peri-code/sdk/view";
import type { Chat, InteractionDTO } from "../../shared";
import { syncMetadataSchema, turnPresentationFacts, type SyncMetadata, type SyncState } from "../../shared";
import { materializeSyncState } from "../../shared";
import {
  normalizeAskUserQuestionDecision, parseAskUserQuestionElicitation,
} from "./askUserQuestion";
import { logError } from "../logging";

/** 应用侧运行元数据；展示身份/时间/错误由 SDK Entry 提供，不再复制。 */
type RunState = { chat: Chat; running: boolean; executionBlocked: boolean };

/** 驻留运行期的只读摘要；`running` 由运行期自身给出，其余来自投影内存态。 */
export interface ProjectionSummary {
  pendingInteractionCount: number;
  lastTurnStatus: string | null;
  lastCompletedAt: string | null;
}

export class ChatProjection {
  private chatDocs = new SessionDocs();
  private chatView = new SessionViewStore(this.chatDocs.chat, this.chatDocs.session);
  private runState?: RunState;
  private error?: string;
  private published = "";
  private commandInputDelivered = false;
  private metadataDirty = true;
  private metadata?: SyncMetadata;
  /** 每次发布递增；相同内容重复发布也递增，代表一次新的投影事实。 */
  private revision = 0;
  /** 本投影实例的稳定身份；运行期重建后变化。 */
  private readonly projectionEpoch = crypto.randomUUID();
  /** AskUserQuestion 的本地待答记录：不写 Yjs，随本轮结束/宿主关闭撤销。 */
  private readonly questions = new Map<string, InteractionDTO>();
  private readonly questionAborts = new Map<string, AbortController>();
  private disposed = false;
  private readonly listeners = new Set<() => void>();

  constructor() {
    this.chatView.subscribe(() => this.publish());
  }

  /** 当前文档对（转换导入后会被替换，订阅者必须经此读取）。 */
  get docs(): SessionDocs { return this.chatDocs; }

  /** 投影每次发布后的通知；订阅者随后应经 `state()` 重新物化（同一轮多次写入会合并）。 */
  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** 丢弃当前展示材料：重放复原重试时使用，避免失败尝试的残留与本次重放叠加。 */
  reset(): void {
    this.abortQuestions();
    this.replaceDocs(new SessionDocs());
  }

  /** 运行期被回收：释放文档（内存态，无外部副作用）。 */
  dispose(): void {
    this.disposed = true;
    this.abortQuestions();
    this.chatView.destroy();
    this.chatDocs.destroy();
  }

  /**
   * 反向 ACP 交互（权限/提问）：AskUserQuestion 走问题卡，其余 elicitation 由 SDK 通用表单处理；
   * 未提供回调时保持默认拒绝。
   */
  handleRequest(method: string, params: unknown, sessionId: string | undefined, callbacks: InteractionCallbacks): Promise<unknown> {
    if (this.disposed) return Promise.reject(new Error("ChatProjection is disposed"));
    if (method === "elicitation/create") {
      const request = params !== null && typeof params === "object" && !Array.isArray(params)
        ? params as Record<string, unknown> : null;
      const requestSession = typeof request?.sessionId === "string" ? request.sessionId : null;
      if (!requestSession || (sessionId && requestSession !== sessionId))
        return Promise.reject(new Error("ACP interaction has an invalid sessionId"));
      const parsed = parseAskUserQuestionElicitation(params);
      if (parsed.kind === "invalid") {
        logError("Invalid AskUserQuestion elicitation", new Error(parsed.reason));
        return Promise.resolve({ action: "decline" });
      }
      if (parsed.kind === "ask-user")
        return this.handleAskUserQuestion(parsed.request, requestSession, callbacks);
    }
    return this.chatDocs.handleRequest(method, params, sessionId, callbacks);
  }

  /** AskUserQuestion：先发布本地待答记录，再等待回答；任何失败都按 decline 结算。 */
  private async handleAskUserQuestion(
    request: { message: string; requestedSchema: Record<string, unknown>; questions: InteractionDTO["questions"] },
    sessionId: string,
    callbacks: InteractionCallbacks,
  ) {
    const questions = request.questions ?? [];
    const interactionId = `interaction:${crypto.randomUUID()}`;
    const abort = new AbortController();
    const interaction: InteractionDTO = {
      id: interactionId, kind: "elicitation", sessionId, status: "pending",
      createdAt: new Date().toISOString(), resolvedAt: null,
      title: "需要你的回答", message: request.message, questions,
    };
    this.questions.set(interactionId, interaction);
    this.questionAborts.set(interactionId, abort);
    this.publish();
    let decision: ReturnType<typeof normalizeAskUserQuestionDecision> = { action: "decline" };
    try {
      const answer = await callbacks.onElicitation?.({
        interactionId, sessionId, message: request.message,
        requestedSchema: request.requestedSchema, signal: abort.signal,
      });
      decision = normalizeAskUserQuestionDecision(answer, questions);
    } catch (error) {
      logError("AskUserQuestion elicitation failed", error, { interactionId });
    } finally {
      this.questions.delete(interactionId);
      this.questionAborts.delete(interactionId);
      abort.abort();
      this.publish();
    }
    return decision;
  }

  /** 本轮结束/复原/回收：待答问题一律失效（回答通道拿不到结果时按 decline 收口）。 */
  private abortQuestions(): void {
    for (const abort of this.questionAborts.values()) abort.abort();
    this.questionAborts.clear();
    this.questions.clear();
  }

  /** 冷加载响应里的模式/配置项进入会话读面。 */
  seedConfig(response: unknown): void { this.chatDocs.seedConfig(response); }

  /** 重放没有独立 turn-done：收到回放内容后按完成收敛，否则历史消息会停留在 streaming。 */
  closeReplayedTurn(): void { this.chatDocs.completeTurn(); }

  /** 任务快照修复入口（session/bg-tasks）；未接线时保持 null，不做无依据的修复。 */
  setTaskSnapshotRequester(requester: (() => Promise<unknown>) | null): void {
    this.chatDocs.setTaskSnapshotRequester(requester);
  }

  setRunState(patch: RunState): void {
    this.runState = patch;
    this.metadataDirty = true;
    this.publish();
  }

  async start(content: string): Promise<void> {
    this.error = undefined;
    this.commandInputDelivered = true;
    this.chatDocs.acceptDeliveredUserInput(crypto.randomUUID(), content);
    await this.flush();
  }

  accept(notification: JsonRpcNotification): void {
    const params = notification.params as { update?: { sessionUpdate?: unknown } } | undefined;
    if (this.commandInputDelivered && notification.method === "session/update"
      && params?.update?.sessionUpdate === "user_message_chunk") return;
    if (notification.method !== "session/update"
      || !["agent_message_chunk", "agent_thought_chunk"].includes(String(params?.update?.sessionUpdate))) {
      this.metadataDirty = true;
    }
    this.chatDocs.accept(notification);
  }

  async complete(status: "completed" | "cancelled" | "error", error?: string): Promise<void> {
    this.abortQuestions();
    this.error = error;
    this.commandInputDelivered = false;
    this.chatDocs.completeTurn(status);
    await this.flush();
  }

  async flush(): Promise<void> {
    await Promise.resolve();
    this.metadataDirty = true;
    this.publish();
  }

  /** 投影读面的物化入口：`state()` 与消费方共用同一 DTO 与身份。 */
  state(): SyncState {
    const metadata = this.metadata ?? {
      chat: this.runState?.chat ?? { id: "", title: "", updatedAt: "" },
      running: this.runState?.running === true, executionBlocked: this.runState?.executionBlocked === true,
      revision: this.revision, projectionEpoch: this.projectionEpoch,
      ...(this.error ? { error: this.error } : {}),
    };
    return this.withPendingQuestions(materializeSyncState(this.chatView.getSnapshot(), metadata));
  }

  /**
   * 只读内存摘要：不物化完整展示状态，供清单合并使用。
   * 未就绪的文档没有可确认的轮次事实，返回 null 而不是猜测。
   */
  summary(): ProjectionSummary {
    const view = this.chatView.getSnapshot();
    const ready = view.readiness === "ready";
    const facts = ready
      ? turnPresentationFacts(view.turns, view.activeTurn.turnId)
      : { current: undefined, lastCompletedAt: null };
    return {
      pendingInteractionCount: this.questions.size + (ready ? view.pendingInteractions.length : 0),
      lastTurnStatus: facts.current?.status ?? null,
      lastCompletedAt: facts.lastCompletedAt,
    };
  }

  /** 待回答交互数：本地 AskUserQuestion + SDK 已发布交互。 */
  get pendingInteractionCount(): number { return this.summary().pendingInteractionCount; }

  /** 待答问题在 SDK 视图之上追加；回答/失效后由本地表移除。 */
  private withPendingQuestions(state: SyncState): SyncState {
    if (this.questions.size === 0) return state;
    const pending = [...this.questions.values()];
    return {
      ...state,
      interactions: [...state.interactions, ...pending],
      pendingInteractions: [...state.pendingInteractions, ...pending],
    };
  }

  /** 更换文档对：视图随新文档重建。 */
  private replaceDocs(docs: SessionDocs): void {
    this.chatView.destroy();
    this.chatDocs = docs;
    this.chatView = new SessionViewStore(docs.chat, docs.session);
    this.chatView.subscribe(() => this.publish());
    this.metadataDirty = true;
    this.published = "";
  }

  private publish(): void {
    if (this.disposed || !this.runState) return;
    this.revision += 1;
    const metadata: SyncMetadata = {
      chat: this.runState.chat, running: this.runState.running, executionBlocked: this.runState.executionBlocked,
      revision: this.revision, projectionEpoch: this.projectionEpoch,
      ...(this.error ? { error: this.error } : {}),
    };
    this.metadata = metadata;
    // revision 每次发布都递增，不能参与「内容是否变化」的比较，否则写回 Yjs 会再次触发发布。
    const serialized = JSON.stringify({
      chat: metadata.chat, running: metadata.running, executionBlocked: metadata.executionBlocked,
      ...(metadata.error ? { error: metadata.error } : {}),
    });
    if (this.metadataDirty || serialized !== this.published) {
      this.metadataDirty = false;
      this.published = serialized;
      this.chatDocs.session.getMap("peri-cf").set("state", syncMetadataSchema.parse(metadata));
    }
    // 展示条目由 `state()` 实时物化；这里只负责把变更通知出去。
    for (const listener of [...this.listeners]) listener();
  }
}
