import {
  ChatCloseIncompleteError,
  ChatViewRegistry,
  type ChatManagerContract,
  type ChatServiceContract,
  type ChatSessionDirectoryEntry,
} from "@peri-code/sdk";

/**
 * 服务端宿主：具体 `ChatManagerContract` + 宿主的 `ChatViewRegistry`。
 *
 * 只负责句柄目录转发与 View 生命周期编排；句柄、绑定、Action 与关闭语义都在 SDK。
 * 这是具体实现，不提供替换端口或注册表的注入 seam；测试直接构造本类。
 */
export class ChatHost {
  readonly views = new ChatViewRegistry();

  constructor(readonly chats: ChatManagerContract) {}

  createChat(options?: { path?: string }): ChatServiceContract {
    return this.chats.createChat(options);
  }

  getChat(id: string): ChatServiceContract | undefined {
    return this.chats.getChat(id);
  }

  findChatBySession(sessionId: string): ChatServiceContract | undefined {
    return this.chats.findChatBySession(sessionId);
  }

  /** 目录查询只读 Store，不启动历史运行实例。 */
  listSessions(options?: { path?: string }): Promise<ChatSessionDirectoryEntry[]> {
    return this.chats.listSessions(options);
  }

  /** 成功关闭后同时释放该 Chat 的视图；任一步失败都保留句柄/占用或显式上报。 */
  async closeChat(id: string): Promise<void> {
    await this.chats.closeChat(id);
    try {
      await this.views.dispose(id);
    } catch (error) {
      throw new ChatCloseIncompleteError(
        [error],
        "Chat is closed but view disposal is incomplete",
      );
    }
  }

  /**
   * 句柄已移除、View 仍待释放：DELETE 重试的残留下沉入口。
   *
   * 在线 Chat 由 `getChat`/`findChatBySession` 解析，不会命中这里；只暴露是否存在，
   * 不把 View 交给 HTTP 层。View 释放成功后条目会被删除，信号自然消失。
   */
  hasPendingClose(chatId: string): boolean {
    return this.chats.getChat(chatId) === undefined
      && this.views.get(chatId) !== undefined;
  }

  async shutdown(): Promise<void> {
    const failures: unknown[] = [];
    try {
      await this.views.disposeAll();
    } catch (error) {
      failures.push(error);
    }
    try {
      await this.chats.shutdown();
    } catch (error) {
      failures.push(error);
    }
    if (failures.length > 0) {
      throw new AggregateError(
        failures,
        "Peri CF chat manager shutdown is incomplete",
      );
    }
  }
}
