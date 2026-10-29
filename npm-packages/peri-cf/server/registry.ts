import { isAbsolute, relative, sep } from "node:path";
import {
  ChatCloseIncompleteError,
  ChatViewRegistry,
  WorkspaceMismatchError,
  type ChatAgentConfig,
  type ChatDatabase,
  type ChatManagerContract,
  type ChatServiceContract,
  type ChatSessionDirectoryEntry,
  type ChatSessionOptions,
  type WasmRuntimeInstanceSnapshot,
  type WorkspaceRef,
} from "@peri-code/sdk";
import { SESSION_CWD, WORKSPACE_CWD } from "./config";
import {
  ChatInstanceMetricsCollector,
  type InstanceBusinessSnapshot,
} from "./instance-metrics";
import {
  DEFAULT_PROJECT_ID,
  ProjectNotFoundError,
  defaultProjectView,
  type ProjectRecord,
  type ProjectStore,
  type ProjectView,
} from "./projects";
import {
  listSandboxWorkspaces,
  parseSandboxWorkspace,
  type SandboxLease,
  type SandboxPoolPort,
  type SandboxPoolSnapshot,
} from "./sandbox";

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

/** 创建 Chat 的两种入口：旧 `path` 或目标会话 id（用于解析该会话的 sandbox）。 */
export interface ChatTarget {
  path?: string;
  sessionId?: string;
  /** 目标项目；`default` 与缺省等价，表示新会话使用独立 sandbox。 */
  projectId?: string;
}

/** 注册表装配依赖；`sandbox` 缺省表示禁用每会话 sandbox。 */
export interface ChatRegistryOptions {
  /** 共享 Session 配置（instructions + 远程 MCP）；sandbox MCP 在创建时合并。 */
  session: ChatSessionOptions;
  /** Store 直连：历史会话的 cwd 是 sandbox 归属的唯一来源。 */
  sessions: Pick<ChatDatabase, "getSession">;
  sandbox?: SandboxPoolPort;
  /** 与 `sandbox` 同源；只用于把 cwd 解析回 `<root>/<sandboxId>/workspace`。 */
  sandboxesRoot?: string;
  /** 项目清单；未装配时所有会话归入默认项目。 */
  projects?: ProjectStore;
}

/** 会话目录条目 + 项目归属；`projectId` 由 cwd 推导，不写入 Store。 */
export interface ProjectSessionEntry extends ChatSessionDirectoryEntry {
  projectId: string;
}

/** 监控读面：实例快照 + 当前会话 + 业务计数；业务计数不可用时为 null。 */
export interface MonitorInstanceView extends WasmRuntimeInstanceSnapshot {
  chatId: string;
  sessionId: string | null;
  business: InstanceBusinessSnapshot | null;
}

export interface MonitorRuntimeView {
  instances: MonitorInstanceView[];
  starting: number;
  unconfirmed: boolean;
}

/**
 * 服务端注册表：持有 SDK `ChatManagerContract` 句柄、`ChatViewRegistry` 与
 * 每 Chat 的 sandbox 租约。
 *
 * 负责关闭/停机编排；句柄、绑定、Action 与关闭语义都在 SDK。
 * 这是具体实现，不是可替换端口；测试直接构造本类。
 */
export class ChatRegistry {
  readonly views = new ChatViewRegistry();
  /** chatId → 租约；只有成功建 Chat 的 sandbox 才登记。 */
  private readonly chatSandboxes = new Map<string, SandboxLease>();
  /** 已登记且未关闭的句柄；用于只读监控，不代表运行后端已启动。 */
  private readonly chatIds = new Set<string>();
  /** 每 Chat 的事实流计数器；只保留数值，不保存通知原文。 */
  private readonly instanceMetrics = new ChatInstanceMetricsCollector();

  /**
   * `workspace` 是旧固定工作区，`agent` 是进程级运行配置模板；SDK 每次
   * `createChat` 都基于模板独立组装（并注入 Manager 的 store 与
   * `workspace.runtimePath` 派生的 cwd），因此同一对象可跨 Chat 复用。
   */
  constructor(
    readonly chats: ChatManagerContract,
    private readonly workspace: WorkspaceRef,
    private readonly agent: ChatAgentConfig,
    private readonly options: ChatRegistryOptions,
  ) {}

  /**
   * 创建句柄的唯一入口：HTTP 层只决定目标，不重复构造模型/工具面配置。
   *
   * 目标会话属于某个 sandbox 时先启动/复用该实例，并把它的 MCP 合并进
   * Session 配置；旧 `/workspace` 会话与显式 `path` 继续走固定工作区。
   */
  async createChat(target: ChatTarget = {}): Promise<ChatServiceContract> {
    if (target.path !== undefined) {
      return this.track(this.createLegacyChat(toSessionPath(this.workspace, target.path)));
    }
    if (target.sessionId !== undefined) {
      const leased = await this.leaseForSession(target.sessionId);
      if (leased) return this.track(await this.createSandboxedChat(leased.lease, leased.sessionPath));
      return this.track(this.createLegacyChat(DEFAULT_SESSION_PATH));
    }
    if (target.projectId !== undefined && target.projectId !== DEFAULT_PROJECT_ID) {
      const lease = await this.leaseForProject(target.projectId);
      return this.track(await this.createSandboxedChat(lease));
    }
    if (this.options.sandbox) {
      const lease = await this.options.sandbox.acquire();
      return this.track(await this.createSandboxedChat(lease));
    }
    return this.track(this.createLegacyChat(DEFAULT_SESSION_PATH));
  }

  /**
   * 活动句柄数：已创建、尚未成功关闭的 Chat。
   *
   * 这是运行后端数量的上限而非实际数量：运行后端在首次 Action 才懒启动。
   */
  activeChatCount(): number {
    return this.chatIds.size;
  }

  /** sandbox 池的只读快照；未启用每会话 sandbox 时返回 `undefined`。 */
  sandboxSnapshot(): SandboxPoolSnapshot | undefined {
    return this.options.sandbox?.snapshot();
  }

  /**
   * WASM 实例读面：实例身份来自 SDK 只读快照，业务计数来自事实流。
   *
   * 同一 Chat 在会话切换窗口可能短暂出现两条租约；只保留最新一条，
   * 避免把旧 generation 标成新 session。没有 chatId 的租约不进入本应用的实例列表。
   */
  runtimeInstances(nowMs: number): MonitorRuntimeView {
    const pool = this.chats.runtimeSnapshot?.()
      ?? { instances: [], starting: 0, unconfirmed: false };
    const latest = new Map<string, WasmRuntimeInstanceSnapshot>();
    for (const instance of pool.instances) {
      const chatId = instance.chatId;
      if (chatId === null) continue;
      const existing = latest.get(chatId);
      if (existing === undefined || instance.startedAtMs > existing.startedAtMs) {
        latest.set(chatId, instance);
      }
    }
    const instances: MonitorInstanceView[] = [...latest.values()].map((instance) => {
      const chatId = instance.chatId as string;
      return {
        ...instance,
        chatId,
        sessionId: this.chats.getChat(chatId)?.sessionId ?? null,
        business: this.instanceMetrics.snapshot(chatId, nowMs),
      };
    });
    return { instances, starting: pool.starting, unconfirmed: pool.unconfirmed };
  }

  private track(chat: ChatServiceContract): ChatServiceContract {
    this.chatIds.add(chat.id);
    this.instanceMetrics.track(chat);
    return chat;
  }

  /**
   * 会话目录：旧 `/workspace` 与每个 sandbox 工作区合并。
   *
   * Store 的目录 SQL 按 cwd 精确匹配，因此必须逐个 sandbox 查询；sandbox 数量
   * 等于本机历史会话数，量级由部署决定。sandbox 目录不存在按空处理。
   */
  async listSessions(path?: string): Promise<ProjectSessionEntry[]> {
    if (path !== undefined) return await this.withProjectIds(await this.chats.listSessions({ path }));
    const merged = new Map<string, ChatSessionDirectoryEntry>();
    for (const entry of await this.chats.listSessions()) merged.set(entry.sessionId, entry);
    if (this.options.sandbox && this.options.sandboxesRoot) {
      for (const workspace of await listSandboxWorkspaces(this.options.sandboxesRoot)) {
        for (const entry of await this.chats.listSessions({ path: workspace })) {
          if (!merged.has(entry.sessionId)) merged.set(entry.sessionId, entry);
        }
      }
    }
    const sorted = [...merged.values()].sort((a, b) =>
      b.updatedAt.localeCompare(a.updatedAt) || b.sessionId.localeCompare(a.sessionId));
    return await this.withProjectIds(sorted);
  }

  /** 项目读面：默认项目始终存在，其余来自清单。 */
  async listProjects(): Promise<ProjectView[]> {
    const store = this.options.projects;
    if (!store) return [defaultProjectView()];
    return await store.listViews();
  }

  /** 名称校验与冲突在 ProjectStore 内完成；HTTP 层只做错误映射。 */
  async createProject(rawName: unknown): Promise<ProjectView> {
    const store = this.options.projects;
    if (!store) throw new Error("Project store is not configured");
    return await store.create(rawName);
  }

  /** 给目录条目补项目归属；Store 目录不带项目字段，归属只由 cwd 派生。 */
  private async withProjectIds(
    entries: ChatSessionDirectoryEntry[],
  ): Promise<ProjectSessionEntry[]> {
    const { projects, sandboxesRoot } = this.options;
    if (!projects || !sandboxesRoot) {
      return entries.map((entry) => ({ ...entry, projectId: DEFAULT_PROJECT_ID }));
    }
    const records: ProjectRecord[] = await projects.list();
    return entries.map((entry) => ({
      ...entry,
      projectId: projects.projectIdForCwd(entry.cwd, records, sandboxesRoot),
    }));
  }

  /** 成功关闭后释放该 Chat 的视图与 sandbox 租约；任一步失败都显式上报。 */
  async closeChat(id: string): Promise<void> {
    await this.chats.closeChat(id);
    this.chatIds.delete(id);
    this.instanceMetrics.forget(id);
    const failures: unknown[] = [];
    try {
      await this.views.dispose(id);
    } catch (error) {
      failures.push(error);
    }
    const lease = this.chatSandboxes.get(id);
    if (lease) {
      try {
        await lease.release();
        this.chatSandboxes.delete(id);
      } catch (error) {
        failures.push(error);
      }
    }
    if (failures.length > 0) {
      throw new ChatCloseIncompleteError(
        failures,
        "Chat is closed but view or sandbox cleanup is incomplete",
      );
    }
  }

  /**
   * 句柄已移除、View 或 sandbox 仍待释放：DELETE 重试的残留下沉入口。
   *
   * 在线 Chat 由 `chats.getChat`/`chats.findChatBySession` 解析，不会命中这里；
   * 只暴露是否存在，不把 View/租约交给 HTTP 层。释放成功后条目会被删除，信号自然消失。
   */
  hasPendingClose(chatId: string): boolean {
    return this.chats.getChat(chatId) === undefined
      && (this.views.get(chatId) !== undefined || this.chatSandboxes.has(chatId));
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
    try {
      await this.options.sandbox?.closeAll();
    } catch (error) {
      failures.push(error);
    }
    this.chatIds.clear();
    this.instanceMetrics.disposeAll();
    if (failures.length > 0) {
      throw new AggregateError(
        failures,
        "Peri CF chat manager shutdown is incomplete",
      );
    }
  }

  private createLegacyChat(sessionPath: string): ChatServiceContract {
    return this.chats.createChat({
      workspace: this.workspace,
      sessionPath,
      agent: this.agent,
    });
  }

  private async createSandboxedChat(
    lease: SandboxLease,
    sessionPath?: string,
  ): Promise<ChatServiceContract> {
    const session: ChatSessionOptions = {
      ...this.options.session,
      mcpServers: {
        ...this.options.session.mcpServers,
        [lease.mcp.name]: lease.mcp.config,
      },
    };
    let chat: ChatServiceContract;
    try {
      chat = this.chats.createChat({
        workspace: lease.workspace,
        ...(sessionPath === undefined ? {} : { sessionPath }),
        agent: this.agent,
        session,
      });
    } catch (error) {
      await lease.release().catch(() => undefined);
      throw error;
    }
    this.chatSandboxes.set(chat.id, lease);
    return chat;
  }

  /**
   * 项目 → sandbox 租约。
   *
   * 首次使用时绑定新 sandbox 并写回清单；并发首次创建会竞争绑定，落败方
   * 释放自己的租约并改用已绑定的 sandbox，保证一个项目只有一个工作区。
   */
  private async leaseForProject(projectId: string): Promise<SandboxLease> {
    const { sandbox, projects } = this.options;
    if (!sandbox) throw new Error("Sandbox pool is not configured");
    if (!projects) throw new Error("Project store is not configured");
    const project = await projects.get(projectId);
    if (!project) throw new ProjectNotFoundError(`项目不存在：${projectId}`);
    if (project.sandboxId !== null) return await sandbox.acquire(project.sandboxId);

    const lease = await sandbox.acquire();
    let assigned;
    try {
      assigned = await projects.assignSandbox(project.id, lease.sandboxId);
    } catch (error) {
      await lease.release().catch(() => undefined);
      throw error;
    }
    if (assigned.sandboxId === null) {
      await lease.release().catch(() => undefined);
      throw new Error(`项目 ${project.id} 未绑定 sandbox`);
    }
    if (assigned.sandboxId === lease.sandboxId) return lease;
    await lease.release().catch(() => undefined);
    return await sandbox.acquire(assigned.sandboxId);
  }

  /** 历史会话 → sandbox 租约；旧 `/workspace` 会话返回 `undefined`。 */
  private async leaseForSession(
    sessionId: string,
  ): Promise<{ lease: SandboxLease; sessionPath?: string } | undefined> {
    const { sandbox, sandboxesRoot } = this.options;
    if (!sandbox || !sandboxesRoot) return undefined;
    const entry = await this.options.sessions.getSession(sessionId);
    if (!entry) return undefined;
    const parsed = parseSandboxWorkspace(sandboxesRoot, entry.cwd);
    if (!parsed) return undefined;

    const lease = await sandbox.acquire(parsed.sandboxId);
    const sessionPath = relative(lease.workspace.runtimePath, entry.cwd);
    if (sessionPath === "") return { lease };
    if (sessionPath === ".." || sessionPath.startsWith(`..${sep}`) || isAbsolute(sessionPath)) {
      await lease.release().catch(() => undefined);
      throw new WorkspaceMismatchError(
        `Session cwd "${entry.cwd}" is outside sandbox workspace "${lease.workspace.runtimePath}"`,
      );
    }
    return { lease, sessionPath: sessionPath.split(sep).join("/") };
  }
}
