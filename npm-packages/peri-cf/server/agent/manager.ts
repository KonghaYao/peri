import {
  ChatCloseIncompleteError,
  ChatViewRegistry,
  type ChatManagerContract,
  type ChatServiceContract,
  type ChatSessionDirectoryEntry,
} from "@peri-code/sdk";
import type { ServerConfig } from "../config";
import { createSdkChatManager, type SdkChatManagerOptions } from "./compose";

/**
 * 服务端编排层：注入的 `ChatManagerContract` + 宿主的 `ChatViewRegistry`。
 *
 * 平台依赖（WASM 运行后端、Store、ownership KV、默认配置）由 `./compose` 装配成
 * 具体实现后注入；这里只负责句柄目录的转发与 View 生命周期编排。
 */

/** 编排层端口：`chats` 必填，`views` 缺省时新建空注册表。 */
interface AppChatManagerOptions {
  readonly chats: ChatManagerContract;
  readonly views?: ChatViewRegistry;
}

/**
 * CF 的进程级编排器：SDK `ChatManager` 端口 + 视图注册表。
 *
 * `shutdown` 先释放 View（断开同步连接），再让 SDK 聚合关闭句柄与运行后端；
 * 两步失败都上报，失败时保留编排器供再次重试。
 */
export class AppChatManager {
  private readonly chats: ChatManagerContract;
  readonly views: ChatViewRegistry;

  constructor(options: AppChatManagerOptions) {
    this.chats = options.chats;
    this.views = options.views ?? new ChatViewRegistry();
  }

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

let processManager: AppChatManager | undefined;

/** 服务端入口的装配选项；`chats` 省略时由 `createSdkChatManager` 构造默认实现。 */
export interface ServerChatOptions extends SdkChatManagerOptions {
  /** 生产/测试替换实现；省略时装配 SDK 默认 `ChatManager`。 */
  readonly chats?: ChatManagerContract;
  /** 测试替换共享注册表；省略时由编排层新建。 */
  readonly views?: ChatViewRegistry;
}

/**
 * 进程级单例：同一进程只创建一个编排器，这是唯一的注入点。
 *
 * 首次调用才构造依赖；WASM 宿主仍由首次激活懒启动。测试用 `shutdownChatManager`
 * 隔离，生产入口只在 `server/index.ts` 调用。
 */
export function getChatManager(
  config: ServerConfig,
  options: ServerChatOptions = {},
): AppChatManager {
  processManager ??= new AppChatManager({
    chats: options.chats ?? createSdkChatManager(config, options),
    views: options.views,
  });
  return processManager;
}

/** 关闭成功后清空单例；失败保留，允许再次 shutdown。 */
export async function shutdownChatManager(): Promise<void> {
  const manager = processManager;
  if (!manager) return;
  await manager.shutdown();
  processManager = undefined;
}
