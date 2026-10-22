import {
  ChatCommandError, defaultDefer, publicError,
  type ChatCommand, type ChatCommandPayload, type ChatRuntimePort, type ChatRuntimeSubscription, type Defer,
} from "@core/agent";
import { commandResponseSchema, type SyncState } from "@core/shared";

const FIRST_SNAPSHOT_TIMEOUT_MS = 30_000;

export type StreamStatus = "connecting" | "synced" | "failed";

function apiUrl(base: string, path: string): string {
  return new URL(path, base || globalThis.location.origin).href;
}

/**
 * 远程会话运行期：投影经 SSE 推送，命令经 HTTP 提交；服务端在重连后重发完整快照。
 * `watch` 先挂上本地订阅再等待 SSE 首帧，最后一个订阅者离开即关闭连接，
 * 避免切换会话后留下后台事件流。
 */
export class RemoteChatRuntime implements ChatRuntimePort {
  private source?: EventSource;
  private snapshot?: SyncState;
  private failure?: unknown;
  private first?: Promise<SyncState>;
  private resolveFirst?: (state: SyncState) => void;
  private rejectFirst?: (error: unknown) => void;
  private timer?: ReturnType<typeof setTimeout>;
  private disposed = false;
  private readonly listeners = new Set<(state: SyncState) => void>();
  private readonly statusListeners = new Set<(status: StreamStatus, error?: Error) => void>();

  constructor(private readonly chatId: string, private readonly base = "",
    private readonly defer: Defer = defaultDefer) {}

  /** 可选观测入口：AgentManager 用它区分“首帧失败”和“已有内容后断线”。 */
  subscribeStreamStatus(listener: (status: StreamStatus, error?: Error) => void): () => void {
    this.statusListeners.add(listener);
    return () => this.statusListeners.delete(listener);
  }

  private reportStatus(status: StreamStatus, error?: Error): void {
    for (const listener of [...this.statusListeners]) listener(status, error);
  }

  async watch(onState: (state: SyncState) => void): Promise<ChatRuntimeSubscription> {
    if (this.disposed) throw new Error("会话运行期已释放");
    let ready = false;
    const listener = (state: SyncState) => { if (ready) onState(state); };
    this.listeners.add(listener);
    try {
      const initial = await this.start();
      ready = true;
      return { initial: this.snapshot ?? initial, close: () => this.release(listener) };
    } catch (error) {
      this.listeners.delete(listener);
      if (this.listeners.size === 0) this.closeStream();
      throw error;
    }
  }

  async read(): Promise<SyncState> {
    const response = await fetch(apiUrl(this.base, `/api/chats/${encodeURIComponent(this.chatId)}`), {
      headers: { accept: "application/json" },
    });
    const body = await response.json().catch(() => null) as { state?: SyncState; error?: string } | null;
    if (!response.ok || !body?.state)
      throw new Error(body?.error ?? `读取会话失败（HTTP ${response.status}）`);
    // HTTP 读面只对本次调用负责，不改写 SSE 订阅持有的投影。
    return body.state;
  }

  async command(command: ChatCommand): Promise<ChatCommandPayload | undefined> {
    let response: Response;
    try {
      response = await fetch(apiUrl(this.base, `/api/chats/${encodeURIComponent(this.chatId)}/commands`), {
        method: "POST",
        headers: { "content-type": "application/json", accept: "application/json" },
        body: JSON.stringify(command),
      });
    } catch (error) {
      // fetch 拒绝说明请求没有拿到可解释的响应：命令是否生效未知，不能当作明确拒绝。
      throw new ChatCommandError("transport_error", publicError(error), "unknown");
    }
    const body = await response.json().catch(() => null) as unknown;
    const parsed = commandResponseSchema.safeParse(body);
    // 响应不可解析、契约不符，或用错误状态表示成功：都无法证明命令是否生效。
    if (!parsed.success || (parsed.data.ok && !response.ok))
      throw new ChatCommandError("transport_error", `命令提交失败（HTTP ${response.status}）`, "unknown");
    if (!parsed.data.ok)
      throw new ChatCommandError(parsed.data.code, parsed.data.message, parsed.data.disposition);
    return parsed.data.payload;
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.listeners.clear();
    this.closeStream();
  }

  /** 挂上流并返回首帧；已有连接时复用同一次等待，重复订阅不会重开事件流。 */
  private start(): Promise<SyncState> {
    if (this.source && this.first) return this.first;
    this.first = new Promise<SyncState>((resolve, reject) => {
      this.resolveFirst = resolve;
      this.rejectFirst = reject;
    });
    // 关闭/启动异常可能让内部等待先于调用方结算；未观察的拒绝不得升级为全局错误。
    this.defer(this.first);
    this.timer ??= setTimeout(() => {
      this.timer = undefined;
      this.rejectFirst?.(new Error("会话事件流连接超时"));
    }, FIRST_SNAPSHOT_TIMEOUT_MS);

    const source = new EventSource(apiUrl(this.base, `/api/chats/${encodeURIComponent(this.chatId)}/events`));
    this.source = source;
    this.reportStatus("connecting");
    source.addEventListener("state", (event) => {
      try {
        this.apply(JSON.parse((event as MessageEvent).data) as SyncState);
      } catch (error) {
        this.failure = error;
        this.rejectFirst?.(error);
        this.reportStatus("failed", error instanceof Error ? error : new Error(String(error)));
      }
    });
    source.addEventListener("error", () => {
      // 页面隐藏/网络抖动也会触发；EventSource 会自行重连，状态在下一帧恢复。
      const error = new Error("会话事件流连接中断，正在尝试重连。");
      this.reportStatus("failed", error);
      // 首帧之前失败：不要留一个只能等 30s 超时的空骨架。
      if (!this.snapshot) {
        this.failure = error;
        this.rejectFirst?.(error);
      }
    });
    return this.first;
  }

  private release(listener: (state: SyncState) => void): void {
    this.listeners.delete(listener);
    if (this.listeners.size === 0) this.closeStream();
  }

  /** 首帧同时是 `watch` 的初始读面；之后每帧继续推给当前订阅者。 */
  private apply(state: SyncState): void {
    this.snapshot = state;
    this.failure = undefined;
    if (this.timer !== undefined) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
    this.resolveFirst?.(state);
    this.resolveFirst = undefined;
    this.rejectFirst = undefined;
    this.reportStatus("synced");
    for (const listener of this.listeners) listener(state);
  }

  private closeStream(): void {
    this.source?.close();
    this.source = undefined;
    if (this.timer !== undefined) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
    if (this.rejectFirst) this.rejectFirst(new Error("会话连接已释放"));
    this.first = undefined;
    this.resolveFirst = undefined;
    this.rejectFirst = undefined;
  }
}
