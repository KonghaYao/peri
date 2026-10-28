import { readdir } from "node:fs/promises";
import { isAbsolute, join, relative, sep } from "node:path";
import type { WorkspaceRef } from "@peri-code/sdk";
import {
  SandboxManager,
  type SandboxHandleState,
  type SandboxMcpServer,
  type SandboxStatus,
} from "@peri-code/sdk/sandbox";
import { createPeriSandboxAdapter, type PeriSandboxStartOptions } from "peri-sandbox";
import { SANDBOX_SERVER_NAME, type ServerConfig } from "./config";

/**
 * 每个会话一个 sandbox 的宿主装配。
 *
 * 生命周期独立于 Chat：sandbox 由本模块按 `sandboxId` 引用计数，同一会话被多个
 * 句柄（例如冲突接管前的候选句柄）引用时不会因其中一个关闭而误停实例。
 * 工作区路径 `<sandboxesRoot>/<sandboxId>/workspace` 是会话 cwd 的唯一来源。
 */

/** SDK `SandboxManager` 默认的 MCP server key；服务端配置不得占用。 */
const SANDBOX_ID_PATTERN = /^sbx_[0-9a-f]{32}$/;
const WORKSPACE_DIRECTORY = "workspace";

/** HTTP 层可映射的 sandbox 启动失败；不把适配器内部错误直接暴露成 500。 */
export class SandboxStartError extends Error {
  readonly code = "sandbox_start_failed" as const;

  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = "SandboxStartError";
  }
}

/** 一个 Chat 持有的 sandbox 租约；`release` 幂等，失败可重试。 */
export interface SandboxLease {
  readonly sandboxId: string;
  readonly workspace: WorkspaceRef;
  readonly mcp: SandboxMcpServer;
  release(): Promise<void>;
}

/** 一个运行中实例的只读快照；`mcpUrl` 由上层去敏后才可出网。 */
export interface SandboxRuntimeSnapshot {
  sandboxId: string;
  state: SandboxHandleState | "unknown";
  /** 持有该实例的租约数，约等于使用它的 Chat 数。 */
  refs: number;
  startedAtMs: number;
  workspacePath: string;
  mcpUrl: string | null;
  mcpServerName: string | null;
  lastError: { code: string; message: string } | null;
}

/** 池层事实：活动实例与启动中的实例数；读取不会启动任何 sandbox。 */
export interface SandboxPoolSnapshot {
  active: SandboxRuntimeSnapshot[];
  starting: number;
}

/** 可替换端口：测试用假实现，生产用 `SandboxPool`。 */
export interface SandboxPoolPort {
  acquire(sandboxId?: string): Promise<SandboxLease>;
  closeAll(): Promise<void>;
  /** 同步、只读；不得触发启动或 MCP 投影。 */
  snapshot(): SandboxPoolSnapshot;
}

interface BackendHandle {
  readonly sandboxId: string;
  readonly workspace: WorkspaceRef;
  /** SDK handle 的 MCP server key；假后端可省略。 */
  readonly serverName?: string;
  /** 只读状态；假后端可省略，此时快照标记为 `unknown`。 */
  status?(): SandboxStatus;
  mcpServer(): Promise<SandboxMcpServer>;
  close(): Promise<void>;
}

/** `SandboxManager` 与池之间最小的结构边界。 */
interface SandboxBackend {
  start(options: { sandboxId?: string }): Promise<BackendHandle>;
}

class ManagerBackend implements SandboxBackend {
  constructor(private readonly manager: SandboxManager<PeriSandboxStartOptions>) {}

  async start(options: { sandboxId?: string }): Promise<BackendHandle> {
    const handle = await this.manager.start(options);
    const workspace = handle.workspace;
    if (!workspace) {
      throw new SandboxStartError(`Sandbox ${handle.sandboxId} started without a workspace`);
    }
    return {
      sandboxId: handle.sandboxId,
      workspace,
      serverName: handle.serverName,
      status: () => handle.status(),
      mcpServer: () => handle.mcpServer(),
      close: () => handle.close(),
    };
  }
}

interface Entry {
  readonly handle: BackendHandle;
  readonly mcp: Promise<SandboxMcpServer>;
  /** 首次登记时间；复用已有实例时保持不变。 */
  readonly startedAtMs: number;
  refs: number;
}

export class SandboxPool implements SandboxPoolPort {
  private readonly entries = new Map<string, Entry>();
  private readonly starting = new Map<string, Promise<Entry>>();

  constructor(private readonly backend: SandboxBackend) {}

  /** 启动或复用实例；同一 `sandboxId` 的并发 acquire 共享一次启动。 */
  async acquire(sandboxId?: string): Promise<SandboxLease> {
    if (sandboxId !== undefined) {
      const existing = this.entries.get(sandboxId);
      if (existing) {
        existing.refs += 1;
        return this.lease(existing, await existing.mcp);
      }
      let pending = this.starting.get(sandboxId);
      if (pending === undefined) {
        pending = this.start(sandboxId);
        this.starting.set(sandboxId, pending);
        // 只清理自己登记的那次启动；失败由 await pending 的调用方接收。
        void pending.then(
          () => this.clearStarting(sandboxId, pending!),
          () => this.clearStarting(sandboxId, pending!),
        );
      }
      const entry = await pending;
      entry.refs += 1;
      return this.lease(entry, await entry.mcp);
    }
    const entry = await this.start();
    entry.refs += 1;
    return this.lease(entry, await entry.mcp);
  }

  private clearStarting(sandboxId: string, pending: Promise<Entry>): void {
    if (this.starting.get(sandboxId) === pending) this.starting.delete(sandboxId);
  }

  private async start(sandboxId?: string): Promise<Entry> {
    let handle: BackendHandle;
    try {
      handle = await this.backend.start(sandboxId === undefined ? {} : { sandboxId });
    } catch (error) {
      throw error instanceof SandboxStartError
        ? error
        : new SandboxStartError(`Sandbox ${sandboxId ?? "(new)"} failed to start`, { cause: error });
    }

    const existing = this.entries.get(handle.sandboxId);
    if (existing) {
      await handle.close().catch(() => undefined);
      return existing;
    }

    const entry: Entry = { handle, refs: 0, startedAtMs: Date.now(), mcp: handle.mcpServer() };
    this.entries.set(handle.sandboxId, entry);
    try {
      await entry.mcp;
    } catch (error) {
      this.entries.delete(handle.sandboxId);
      await handle.close().catch(() => undefined);
      throw new SandboxStartError(`Sandbox ${handle.sandboxId} did not expose an MCP endpoint`, {
        cause: error,
      });
    }
    return entry;
  }

  /** 引用归零才关闭实例；工作区保留，后续同 id 启动可续用。 */
  async release(sandboxId: string): Promise<void> {
    const entry = this.entries.get(sandboxId);
    if (!entry) return;
    if (entry.refs > 0) entry.refs -= 1;
    if (entry.refs > 0) return;
    await entry.handle.close();
    if (this.entries.get(sandboxId) === entry) this.entries.delete(sandboxId);
  }

  /** 只遍历内存事实：不等待 MCP 投影，也不调用任何会启动实例的方法。 */
  snapshot(): SandboxPoolSnapshot {
    const active = [...this.entries.values()].map((entry): SandboxRuntimeSnapshot => {
      const status = this.statusOf(entry.handle);
      return {
        sandboxId: entry.handle.sandboxId,
        state: status?.state ?? "unknown",
        refs: entry.refs,
        startedAtMs: entry.startedAtMs,
        workspacePath: entry.handle.workspace.runtimePath,
        mcpUrl: status?.mcpUrl ?? null,
        mcpServerName: entry.handle.serverName ?? null,
        lastError: status?.lastError
          ? { code: status.lastError.code, message: status.lastError.message }
          : null,
      };
    });
    active.sort((a, b) => a.sandboxId.localeCompare(b.sandboxId));
    return { active, starting: this.starting.size };
  }

  /** 只读状态失败按 `unknown` 处理；监控读取不能反过来影响实例。 */
  private statusOf(handle: BackendHandle): SandboxStatus | undefined {
    if (!handle.status) return undefined;
    try {
      return handle.status();
    } catch {
      return undefined;
    }
  }

  async closeAll(): Promise<void> {
    const failures: unknown[] = [];
    // 先等启动中的实例落地，避免关闭时遗漏刚登记但尚未完成的条目。
    await Promise.all([...this.starting.values()].map((pending) => pending.catch(() => undefined)));
    await Promise.all([...this.entries.values()].map(async (entry) => {
      try {
        await entry.handle.close();
      } catch (error) {
        failures.push(error);
      }
    }));
    if (failures.length > 0) {
      throw new AggregateError(failures, "Sandbox shutdown is incomplete");
    }
    this.entries.clear();
  }

  private lease(entry: Entry, mcp: SandboxMcpServer): SandboxLease {
    return {
      sandboxId: entry.handle.sandboxId,
      workspace: entry.handle.workspace,
      mcp,
      release: () => this.release(entry.handle.sandboxId),
    };
  }
}

/** 生产装配：适配器只在启用时构造，禁用时完全不触碰 Peri/SRT。 */
export function createSandboxPool(config: ServerConfig): SandboxPool | undefined {
  if (!config.sandbox.enabled) return undefined;
  const adapter = createPeriSandboxAdapter({
    sandboxesRoot: config.sandbox.root,
    ...(config.sandbox.settingsPath === undefined
      ? {}
      : { settingsPath: config.sandbox.settingsPath }),
  });
  const manager = new SandboxManager<PeriSandboxStartOptions>({
    adapter,
    serverName: SANDBOX_SERVER_NAME,
    ...(config.sandbox.profileName === undefined
      ? {}
      : { adapterOptions: { profileName: config.sandbox.profileName } }),
  });
  return new SandboxPool(new ManagerBackend(manager));
}

/**
 * 从会话 cwd 反推 sandbox 与相对 `sessionPath`。
 *
 * 只接受 `<root>/sbx_<32hex>/workspace[/...]`；`..`、绝对路径和未知目录都返回
 * `null`，由调用方回退到旧工作区（历史 `/workspace` 会话仍可打开）。
 */
export function parseSandboxWorkspace(
  root: string,
  cwd: string,
): { sandboxId: string; sessionPath?: string } | null {
  if (!isAbsolute(root) || !isAbsolute(cwd)) return null;
  const rel = relative(root, cwd);
  if (rel === "" || rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel)) return null;
  const [sandboxId, workspaceDirectory, ...rest] = rel.split(sep);
  if (!SANDBOX_ID_PATTERN.test(sandboxId) || workspaceDirectory !== WORKSPACE_DIRECTORY) return null;
  const sessionPath = rest.join("/");
  return sessionPath.length > 0 ? { sandboxId, sessionPath } : { sandboxId };
}

/** 列出现存 sandbox 的工作区根，供会话目录合并查询；根不存在视为空。 */
export async function listSandboxWorkspaces(root: string): Promise<string[]> {
  if (!isAbsolute(root)) return [];
  let entries;
  try {
    entries = await readdir(root, { withFileTypes: true });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw error;
  }
  return entries
    .filter((entry) => entry.isDirectory() && SANDBOX_ID_PATTERN.test(entry.name))
    .map((entry) => join(root, entry.name, WORKSPACE_DIRECTORY));
}
