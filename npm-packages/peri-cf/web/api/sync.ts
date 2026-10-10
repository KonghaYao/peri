import { decodeSyncFrame, encodeAuthFrame, encodeAckFrame, SessionDocReplica } from '@peri-code/sdk/view';
import { readSyncState, UnsupportedSchemaError } from '../../shared/sync-state';
import { chatCommandResultSchema, encodeCommandFrame, type ChatCommand, type ChatCommandResult } from '../../shared/sync';
import type { SyncState } from '../../shared/sync';

export type { SyncState } from '../../shared/sync';
export type SyncStatus = 'connecting' | 'synced' | 'disconnected' | 'failed';
type Socket = Pick<WebSocket, 'onopen' | 'onmessage' | 'onclose' | 'onerror' | 'send' | 'close' | 'binaryType'>;

export type CommandPayload = NonNullable<ChatCommandResult['payload']>;
export interface CommandOptions { timeoutMs?: number; signal?: AbortSignal }

/**
 * 命令失败：服务端明确答否（`source: 'server'`）说明命令未生效、执行状态是确定的；
 * 客户端未获确认（`source: 'client'`：未同步/超时/断线/中止）时执行状态不确定，调用方必须阻断重跑。
 */
export type CommandFailure = { ok: false; code: string; message: string; source: 'server' | 'client' };
export type CommandOutcome = { ok: true; payload?: CommandPayload } | CommandFailure;

/** 会话命令通道的窄接口；ChatSync 实现它，视图层不直接触碰 socket。 */
export interface ChatCommandPort {
  command(command: ChatCommand, options?: CommandOptions): Promise<CommandOutcome>;
}

export function clientCommandFailure(code: string, message: string): CommandFailure {
  return { ok: false, code, message, source: 'client' };
}

export const COMMAND_TIMEOUT_MS = 15_000;

/** 命令帧 id 只用于匹配结果；同一连接内不重复即可（控制帧 id 上限 64）。 */
export function nextCommandId(): string {
  const random = globalThis.crypto?.randomUUID?.() ?? `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  return random.slice(0, 64);
}

export interface ChatSyncOptions {
  chatId: string;
  token: string;
  onState(state: SyncState): void;
  onStatus(status: SyncStatus, error?: Error): void;
  socket?: (url: string) => Socket;
  origin?: string;
  reconnectDelayMs?: number;
  maxReconnectAttempts?: number;
  readyTimeoutMs?: number;
  commandTimeoutMs?: number;
}

export class ChatSync implements ChatCommandPort {
  private readonly replica = new SessionDocReplica();
  private socket: Socket | undefined;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private handshake: ReturnType<typeof setTimeout> | undefined;
  private unobserve: (() => void) | undefined;
  private disposed = false;
  private applying = false;
  private attempts = 0;
  private status: SyncStatus = 'connecting';
  /** 在途命令：结果帧按 id 匹配；断线/超时/替换 socket 时整体作废，不自动重放。 */
  private pending = new Map<string, (outcome: CommandOutcome) => void>();

  constructor(private readonly options: ChatSyncOptions) {
    this.connect();
  }

  private report(status: SyncStatus, error?: Error) {
    this.status = status;
    this.options.onStatus(status, error);
  }

  private publish() {
    const state = readSyncState(this.replica.chat, this.replica.session);
    // 快照尚未完整：不发布空历史；副本本来就是空的。
    if (state.readiness === 'not-ready') return;
    if (state.chat.id !== this.options.chatId) throw new Error('同步返回了不匹配的会话。');
    this.options.onState(state);
  }

  private observe() {
    this.unobserve?.();
    const observer = () => {
      if (!this.applying && !this.disposed && this.status === 'synced') this.publish();
    };
    const { chat, session } = this.replica;
    chat.on('afterTransaction', observer);
    session.on('afterTransaction', observer);
    this.unobserve = () => {
      chat.off('afterTransaction', observer);
      session.off('afterTransaction', observer);
    };
  }

  private retireSocket(reason = '同步连接已断开，命令未获确认。') {
    clearTimeout(this.handshake);
    // 连接退休即作废在途命令：新连接可能落在别的 isolate，旧命令已无法确认，也绝不自动重放。
    this.settlePending(clientCommandFailure('transport_error', reason));
    const socket = this.socket;
    this.socket = undefined;
    if (socket) {
      socket.onopen = socket.onmessage = socket.onclose = socket.onerror = null;
      socket.close();
    }
  }

  private settlePending(outcome: CommandOutcome) {
    const waiting = [...this.pending.values()];
    this.pending.clear();
    for (const settle of waiting) settle(outcome);
  }

  /**
   * 下发会话命令：只在 `synced` 的这条连接上可用——执行驻留在接受连接的 isolate。
   * 失败一律收敛成结果对象（不抛）：调用方按 `source` 区分「服务端答否」与「未获确认」。
   */
  async command(command: ChatCommand, options: CommandOptions = {}): Promise<CommandOutcome> {
    const socket = this.socket;
    if (this.disposed) return clientCommandFailure('unavailable', '会话已关闭。');
    if (this.status !== 'synced' || !socket) return clientCommandFailure('unavailable', '同步连接未就绪，命令未发送。');
    if (options.signal?.aborted) return clientCommandFailure('aborted', '命令已取消。');
    if (this.pending.has(command.id)) throw new Error('命令 id 重复，无法匹配结果。');
    const timeoutMs = options.timeoutMs ?? this.options.commandTimeoutMs ?? COMMAND_TIMEOUT_MS;
    return await new Promise<CommandOutcome>(resolve => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const finish = (outcome: CommandOutcome) => {
        clearTimeout(timer);
        options.signal?.removeEventListener('abort', onAbort);
        this.pending.delete(command.id);
        resolve(outcome);
      };
      const onAbort = () => finish(clientCommandFailure('aborted', '命令已取消。'));
      options.signal?.addEventListener('abort', onAbort, { once: true });
      timer = setTimeout(() => finish(clientCommandFailure('transport_error', '命令等待服务端确认超时。')), timeoutMs);
      this.pending.set(command.id, finish);
      try { socket.send(encodeCommandFrame(command)); }
      catch { finish(clientCommandFailure('transport_error', '命令发送失败。')); }
    });
  }

  /** 结果帧与在途命令按 id 匹配；未知 id 是超时后迟到的结果，命令已作废，忽略而不是复活。 */
  private settleCommandResult(data: string) {
    const result = chatCommandResultSchema.parse(JSON.parse(data));
    const settle = this.pending.get(result.id);
    if (!settle) return;
    settle(result.ok
      ? { ok: true, ...(result.payload ? { payload: result.payload } : {}) }
      : { ok: false, source: 'server', code: result.error?.code ?? 'unknown', message: result.error?.message ?? '服务端拒绝了该命令。' });
  }

  private fail(error: Error) {
    this.retireSocket();
    clearTimeout(this.timer);
    this.report('failed', error);
  }

  private reconnect(error?: Error) {
    this.retireSocket();
    if (this.disposed) return;
    if (this.attempts >= (this.options.maxReconnectAttempts ?? 5)) {
      this.fail(new Error('同步重连次数已达上限，请重新加载会话。'));
      return;
    }
    this.report('disconnected', error);
    clearTimeout(this.timer);
    const delay = Math.min((this.options.reconnectDelayMs ?? 500) * 2 ** Math.min(this.attempts++, 5), 15_000);
    this.timer = setTimeout(() => this.connect(), delay);
  }

  private connect() {
    if (this.disposed) return;
    this.report('connecting');
    const url = new URL(`/api/v2/chats/${encodeURIComponent(this.options.chatId)}/sync`,
      this.options.origin ?? location.origin);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    let socket: Socket;
    try { socket = (this.options.socket ?? (address => new WebSocket(address)))(url.href); }
    catch { this.reconnect(new Error('同步连接失败。')); return; }
    socket.binaryType = 'arraybuffer';
    this.socket = socket;
    let receivedSnapshot = false;
    let delivery = 0;
    const current = () => !this.disposed && this.socket === socket;
    this.handshake = setTimeout(() => {
      if (current()) this.reconnect(new Error('等待同步快照超时。'));
    }, this.options.readyTimeoutMs ?? 10_000);
    socket.onopen = () => {
      if (!current()) return;
      try {
        socket.send(encodeAuthFrame({ type: 'auth', token: this.options.token, resume: this.replica.stateVector() }));
      } catch { this.fail(new Error('同步认证帧发送失败。')); }
    };
    socket.onmessage = event => {
      if (!current()) return;
      // 文本帧只承载应用控制帧；SDK 文档同步帧一律二进制，两条路径不混用解析。
      if (typeof event.data === 'string') {
        try { this.settleCommandResult(event.data); }
        catch { this.fail(new Error('命令结果帧格式不正确，请重新加载会话。')); }
        return;
      }
      try {
        const frame = decodeSyncFrame(event.data);
        if (frame.delivery !== delivery + 1)
          throw new Error('同步投递序列不连续。');
        this.applying = true;
        try {
          if (frame.type === 'snapshot') {
            this.unobserve?.();
            this.unobserve = undefined;
            this.replica.applySnapshot(frame.snapshot);
            this.observe();
            receivedSnapshot = true;
          } else {
            if (!receivedSnapshot) throw new Error('同步更新缺少快照。');
            try { this.replica.applyUpdate(frame.update); }
            catch { this.reconnect(new Error('同步序列中断，正在修复。')); return; }
          }
        } finally { this.applying = false; }
        this.publish();
        socket.send(encodeAckFrame(frame.delivery));
        delivery = frame.delivery;
        clearTimeout(this.handshake);
        this.attempts = 0;
        this.report('synced');
      } catch (error) {
        // 不受支持的文档版本必须停用并提示升级，不伪装成格式错误、不循环 resume。
        this.fail(error instanceof UnsupportedSchemaError ? error : new Error('同步数据格式不正确，请重新加载会话。'));
      }
    };
    socket.onclose = event => {
      if (!current()) return;
      if ([1008, 1009, 4401, 4403, 4404].includes(event.code)) {
        this.fail(new Error('同步被拒绝，请检查访问令牌或重新加载会话。'));
      } else this.reconnect(new Error('同步连接已断开，正在重新连接。'));
    };
    socket.onerror = () => {
      if (current()) this.reconnect(new Error('同步连接异常，正在重新连接。'));
    };
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    clearTimeout(this.timer);
    this.retireSocket('会话已关闭，命令未获确认。');
    this.unobserve?.();
    this.unobserve = undefined;
    this.replica.destroy();
  }
}
