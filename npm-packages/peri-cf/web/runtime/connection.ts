import type { SyncState } from "../../shared/sync";
import { publicError } from "./logging";
import { ChatCommandError, type ChatCommand, type ChatRuntime } from "./session";

export type { SyncState } from "../../shared/sync";

export type SyncStatus = "connecting" | "synced" | "failed";

export type CommandPayload = { id: string; bytes: number; json: string };
export interface CommandOptions { signal?: AbortSignal }

/**
 * 命令失败：本地执行方的拒绝（`source: 'server'`）说明命令未生效、执行状态确定；
 * 客户端未送达（`source: 'client'`：连接已释放/调用前已中止）时命令未执行，调用方可以安全重试。
 */
export type CommandFailure = { ok: false; code: string; message: string; source: "server" | "client" };
export type CommandOutcome = { ok: true; payload?: CommandPayload } | CommandFailure;

/** 会话命令通道的窄接口；视图层不直接触碰本地宿主。 */
export interface ChatCommandPort {
  command(command: ChatCommand, options?: CommandOptions): Promise<CommandOutcome>;
}

export function clientCommandFailure(code: string, message: string): CommandFailure {
  return { ok: false, code, message, source: "client" };
}

export interface ChatConnectionOptions {
  chatId: string;
  runtime: ChatRuntime;
  onState(state: SyncState): void;
  onStatus(status: SyncStatus, error?: Error): void;
}

/**
 * 本地会话连接：订阅运行期投影并把命令交给本地宿主。命令经内存调用完成——
 * 失败总是「确定未生效」，不存在需要阻断重跑的未确认中间态。
 */
export class ChatConnection implements ChatCommandPort {
  private unobserve?: () => void;
  private disposed = false;

  constructor(private readonly options: ChatConnectionOptions) {
    const { runtime, onStatus } = options;
    this.unobserve = runtime.subscribe(() => this.publish());
    runtime.hydrate().then(
      () => {
        if (this.disposed) return;
        onStatus("synced");
        this.publish();
      },
      (error: unknown) => {
        if (this.disposed) return;
        onStatus("failed", error instanceof Error ? error : new Error(publicError(error)));
      },
    );
  }

  private publish(): void {
    if (this.disposed) return;
    this.options.onState(this.options.runtime.state());
  }

  async command(command: ChatCommand, options: CommandOptions = {}): Promise<CommandOutcome> {
    if (this.disposed) return clientCommandFailure("unavailable", "会话连接已释放，命令未发送。");
    if (options.signal?.aborted) return clientCommandFailure("aborted", "命令已中止，未发送。");
    try {
      const payload = await this.options.runtime.run(command);
      return { ok: true, ...(payload ? { payload } : {}) };
    } catch (error) {
      if (error instanceof ChatCommandError)
        return { ok: false, code: error.code, message: error.message, source: "server" };
      return { ok: false, code: "internal_error", message: publicError(error), source: "server" };
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.unobserve?.();
    this.unobserve = undefined;
  }
}
