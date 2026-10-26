import {
  ChatReplica,
  type ChatReplicaConnection,
  type ChatReplicaStatus,
  type ChatYjsChannel,
} from "@peri-code/sdk/view";
import { materializeChatDisplayState, type Chat, type ChatDisplayState } from "../../shared";

/**
 * 浏览器展示投影：只读 `ChatReplica`（其底层为 SDK `ChatDocument` 文档对）。
 *
 * 不持有服务端投影门面，不反向控制执行。每次读取都经
 * schema 校验；副本未就绪或版本不受支持时保留 `readiness`，不伪造空历史。
 */
export class ChatProjection {
  private readonly replica = new ChatReplica();
  private readonly listeners = new Set<() => void>();
  private readonly unsubscribe: () => void;
  private chat: Chat;
  private cached?: ChatDisplayState;
  private disposed = false;

  constructor(chat: Chat) {
    this.chat = chat;
    this.unsubscribe = this.replica.subscribe(() => {
      this.cached = undefined;
      this.emit();
    });
  }

  get status(): ChatReplicaStatus { return this.replica.status; }

  /** 建立本次只读同步连接；重连前必须等待上一次连接关闭。 */
  connect(channel: ChatYjsChannel): ChatReplicaConnection {
    if (this.disposed) throw new Error("ChatProjection is disposed");
    return this.replica.connect(channel);
  }

  /** 当前展示状态；相同文档代内复用已校验结果。 */
  state(): ChatDisplayState {
    if (!this.cached) this.cached = materializeChatDisplayState(this.replica.read(), this.chat);
    return this.cached;
  }

  subscribe(listener: () => void): () => void {
    if (this.disposed) return () => {};
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    this.unsubscribe();
    this.listeners.clear();
    await this.replica.dispose();
  }

  private emit(): void {
    if (this.disposed) return;
    for (const listener of [...this.listeners]) listener();
  }
}
