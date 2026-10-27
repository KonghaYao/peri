import {
  ChatCloseIncompleteError,
  ChatViewRegistry,
  type ChatAgentConfig,
  type ChatManagerContract,
  type ChatServiceContract,
} from "@peri-code/sdk";

/**
 * 服务端注册表：持有 SDK `ChatManagerContract` 句柄与 `ChatViewRegistry`。
 *
 * 负责关闭/停机编排；句柄、绑定、Action 与关闭语义都在 SDK。
 * 这是具体实现，不是可替换端口；测试直接构造本类。
 */
export class ChatRegistry {
  readonly views = new ChatViewRegistry();

  /**
   * `agent` 是进程级运行配置模板；SDK 每次 `createChat` 都基于它独立组装
   * （并注入 Manager 的 store），因此同一对象可跨 Chat 复用。
   */
  constructor(
    readonly chats: ChatManagerContract,
    private readonly agent: ChatAgentConfig,
  ) {}

  /**
   * 创建句柄的唯一入口：HTTP 层只决定 path，不重复构造模型/工具面配置。
   */
  createChat(path?: string): ChatServiceContract {
    return this.chats.createChat({ agent: this.agent, path });
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
   * 在线 Chat 由 `chats.getChat`/`chats.findChatBySession` 解析，不会命中这里；
   * 只暴露是否存在，不把 View 交给 HTTP 层。View 释放成功后条目会被删除，信号自然消失。
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
