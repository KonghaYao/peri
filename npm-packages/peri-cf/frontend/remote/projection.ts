import {
  ChatReplica,
  type ChatReplicaConnection,
  type ChatReplicaStatus,
  type ChatYjsChannel,
  type SessionView,
} from "@peri-code/sdk/view";
import type { Chat } from "./contracts";
import { materializeChatDisplayState, type ChatDisplayState } from "./display-state";

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
    if (!this.cached) {
      const view = this.replica.read();
      this.cached = materializeChatDisplayState(view, this.chatFor(view));
    }
    return this.cached;
  }

  /**
   * 展示身份以当前绑定为准：`sessionId` 变化时 id 跟着换，标题优先取副本会话事实。
   * 构造时的 Chat 只作为尚未绑定时的占位，不再当作会话身份快照。
   */
  private chatFor(view: SessionView): Chat {
    const id = view.sessionId ?? this.chat.id;
    const title = view.session?.title ?? this.chat.title;
    return id === this.chat.id && title === this.chat.title
      ? this.chat
      : { id, title, updatedAt: this.chat.updatedAt };
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
