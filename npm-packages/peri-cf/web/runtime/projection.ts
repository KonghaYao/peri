import { SessionDocs, type InteractionCallbacks, type JsonRpcNotification } from "@peri-code/sdk/portable";
import { SessionViewStore } from "@peri-code/sdk/view";
import type { Chat } from "../../shared/chat";
import { syncMetadataSchema, type SyncMetadata, type SyncState } from "../../shared/sync";
import { materializeSyncState } from "../../shared/sync-state";

/** 应用侧运行元数据；展示身份/时间/错误由 SDK Entry 提供，不再复制。 */
type RunState = { chat: Chat; running: boolean; executionBlocked: boolean };

export class ChatProjection {
  private chatDocs = new SessionDocs();
  private chatView = new SessionViewStore(this.chatDocs.chat, this.chatDocs.session);
  private runState?: RunState;
  private error?: string;
  private published = "";
  private commandInputDelivered = false;
  private metadataDirty = true;
  private metadata?: SyncMetadata;
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
    this.replaceDocs(new SessionDocs());
  }

  /** 运行期被回收：释放文档（内存态，无外部副作用）。 */
  dispose(): void {
    this.chatView.destroy();
    this.chatDocs.destroy();
  }

  /** 反向 ACP 交互（权限/提问）进入 SDK 投影；未提供回调时保持默认拒绝。 */
  handleRequest(method: string, params: unknown, sessionId: string | undefined, callbacks: InteractionCallbacks): Promise<unknown> {
    return this.chatDocs.handleRequest(method, params, sessionId, callbacks);
  }

  /** 冷加载响应里的模式/配置项进入会话读面。 */
  seedConfig(response: unknown): void { this.chatDocs.seedConfig(response); }

  /**
   * 重放没有独立的 turn-done 通知：收到回放内容后按完成收敛，
   * 否则最后一条历史消息会停留在 streaming、历史 turn 停留在 running（与 SDK Session 同规则）。
   */
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
      ...(this.error ? { error: this.error } : {}),
    };
    return materializeSyncState(this.chatView.getSnapshot(), metadata);
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
    if (!this.runState) return;
    const metadata: SyncMetadata = {
      chat: this.runState.chat, running: this.runState.running, executionBlocked: this.runState.executionBlocked,
      ...(this.error ? { error: this.error } : {}),
    };
    this.metadata = metadata;
    const serialized = JSON.stringify(metadata);
    if (this.metadataDirty || serialized !== this.published) {
      this.metadataDirty = false;
      this.published = serialized;
      this.chatDocs.session.getMap("peri-cf").set("state", syncMetadataSchema.parse(metadata));
    }
    // 展示条目由 `state()` 实时物化；这里只负责把变更通知出去。
    for (const listener of [...this.listeners]) listener();
  }
}
