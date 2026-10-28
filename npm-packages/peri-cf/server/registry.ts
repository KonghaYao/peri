import { isAbsolute, relative, sep } from "node:path";
import {
  ChatCloseIncompleteError,
  ChatViewRegistry,
  WorkspaceMismatchError,
  type ChatAgentConfig,
  type ChatManagerContract,
  type ChatServiceContract,
  type WorkspaceRef,
} from "@peri-code/sdk";
import { SESSION_CWD, WORKSPACE_CWD } from "./config";

/**
 * 默认 `sessionPath`：原 `SESSION_CWD` 相对 `WORKSPACE_CWD` 的路径。
 * 当前两者只差尾斜杠，因此为空串，即工作区根；SDK 会归一化为 `runtimePath`。
 */
const DEFAULT_SESSION_PATH = relative(WORKSPACE_CWD, SESSION_CWD);

/**
 * 把 HTTP 的 `path` 映射为相对 workspace 根的 `sessionPath`。
 *
 * 相对路径原样交给 SDK 解析；绝对路径必须位于 `runtimePath` 下，否则以
 * `workspace_mismatch` 拒绝（`/workspace-other` 不算 `/workspace` 的后代）。
 */
function toSessionPath(workspace: WorkspaceRef, path: string | undefined): string {
  if (path === undefined) return DEFAULT_SESSION_PATH;
  if (!isAbsolute(path)) return path;
  const relativePath = relative(workspace.runtimePath, path);
  if (relativePath === "") return DEFAULT_SESSION_PATH;
  if (
    relativePath === ".."
    || relativePath.startsWith(`..${sep}`)
    || isAbsolute(relativePath)
  ) {
    throw new WorkspaceMismatchError(
      `Session path "${path}" is outside the workspace runtime root "${workspace.runtimePath}"`,
    );
  }
  return relativePath.split(sep).join("/");
}

/**
 * 服务端注册表：持有 SDK `ChatManagerContract` 句柄与 `ChatViewRegistry`。
 *
 * 负责关闭/停机编排；句柄、绑定、Action 与关闭语义都在 SDK。
 * 这是具体实现，不是可替换端口；测试直接构造本类。
 */
export class ChatRegistry {
  readonly views = new ChatViewRegistry();

  /**
   * `workspace` 是服务唯一工作区，`agent` 是进程级运行配置模板；SDK 每次
   * `createChat` 都基于模板独立组装（并注入 Manager 的 store 与
   * `workspace.runtimePath` 派生的 cwd），因此同一对象可跨 Chat 复用。
   */
  constructor(
    readonly chats: ChatManagerContract,
    private readonly workspace: WorkspaceRef,
    private readonly agent: ChatAgentConfig,
  ) {}

  /**
   * 创建句柄的唯一入口：HTTP 层只决定 path，不重复构造模型/工具面配置。
   *
   * `path` 是旧接口的绝对会话 cwd；位于工作区内时转换为相对 `sessionPath`，
   * 空值使用工作区根。其余 workspace 与 agent 语义在 SDK。
   */
  createChat(path?: string): ChatServiceContract {
    return this.chats.createChat({
      workspace: this.workspace,
      sessionPath: toSessionPath(this.workspace, path),
      agent: this.agent,
    });
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
