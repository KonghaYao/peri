import {
  SessionDocs, SessionDocSync, SessionViewStore,
  type DisplayCheckpoint, type InteractionCallbacks, type JsonRpcNotification,
} from "../sdk";
import type { Chat } from "../types";
import { syncMetadataSchema, type SyncMetadata, type SyncState } from "../../shared/sync";
import { materializeSyncState } from "../../shared/sync-state";

/** 应用侧运行元数据；展示身份/时间/错误由 SDK Entry 提供，不再复制。 */
type RunState = { chat: Chat; running: boolean; executionBlocked: boolean };

export class ChatProjection {
  private chatDocs = new SessionDocs();
  private chatSync = new SessionDocSync(this.chatDocs.chat, this.chatDocs.session);
  private chatView = new SessionViewStore(this.chatDocs.chat, this.chatDocs.session);
  private runState?: RunState;
  private error?: string;
  private published = "";
  private commandInputDelivered = false;
  private metadataDirty = true;
  private publishedEntryCount = -1;
  private metadata?: SyncMetadata;

  constructor() {
    this.chatView.subscribe(() => this.publish());
  }

  /** 当前文档对（转换导入后会被替换，订阅者必须经此读取）。 */
  get docs(): SessionDocs { return this.chatDocs; }
  get sync(): SessionDocSync { return this.chatSync; }

  /** 从 v2 checkpoint 导入并替换文档对；活动 turn/interactions 由导入收敛。 */
  restoreCheckpoint(checkpoint: unknown): void {
    const docs = SessionDocs.fromCheckpoint(checkpoint);
    this.replaceDocs(docs);
  }

  /** 当前展示材料（含外置载荷全文），供记录适配器拆分提交。 */
  checkpoint(): DisplayCheckpoint { return this.chatDocs.exportCheckpoint(); }

  /** v1 转换时发现此前正在执行：置应用级错误（不持久化，随记录里的 executionBlocked 一同展示）。 */
  setRestartInterrupted(): void {
    this.error = "Execution interrupted by host restart; not automatically resumed";
  }

  /** 反向 ACP 交互（权限/提问）进入 SDK 投影；未提供回调时保持默认拒绝。 */
  handleRequest(method: string, params: unknown, sessionId: string | undefined, callbacks: InteractionCallbacks): Promise<unknown> {
    return this.chatDocs.handleRequest(method, params, sessionId, callbacks);
  }

  /** 冷加载响应里的模式/配置项进入会话读面。 */
  seedConfig(response: unknown): void { this.chatDocs.seedConfig(response); }

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
    this.chatSync.flush();
  }

  /** 与 WS 实时同源的首屏状态；HTTP 与推送共用同一 DTO 与身份。 */
  state(): SyncState {
    const metadata = this.metadata ?? {
      chat: this.runState?.chat ?? { id: "", title: "", updatedAt: "" },
      running: this.runState?.running === true, executionBlocked: this.runState?.executionBlocked === true,
      ...(this.error ? { error: this.error } : {}),
    };
    return materializeSyncState(this.chatView.getSnapshot(), metadata);
  }

  /** 导入后整体更换文档对：旧的同步通道作废（发布前无订阅者），视图随新文档重建。 */
  private replaceDocs(docs: SessionDocs): void {
    this.chatView.destroy();
    void this.chatSync.close().catch(() => {});
    this.chatDocs = docs;
    this.chatSync = new SessionDocSync(docs.chat, docs.session);
    this.chatView = new SessionViewStore(docs.chat, docs.session);
    this.chatView.subscribe(() => this.publish());
    this.metadataDirty = true;
    this.publishedEntryCount = -1;
    this.published = "";
  }

  private publish(): void {
    if (!this.runState) return;
    const snapshot = this.chatView.getSnapshot();
    if (!this.metadataDirty && this.publishedEntryCount === snapshot.entries.length) return;
    this.metadataDirty = false;
    this.publishedEntryCount = snapshot.entries.length;
    const metadata: SyncMetadata = {
      chat: this.runState.chat, running: this.runState.running, executionBlocked: this.runState.executionBlocked,
      ...(this.error ? { error: this.error } : {}),
    };
    this.metadata = metadata;
    const serialized = JSON.stringify(metadata);
    if (serialized !== this.published) {
      this.published = serialized;
      this.chatDocs.session.getMap("peri-cf").set("state", syncMetadataSchema.parse(metadata));
    }
  }
}
