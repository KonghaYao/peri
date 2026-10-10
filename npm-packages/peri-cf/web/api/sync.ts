import { decodeSyncFrame, encodeAuthFrame, encodeAckFrame, SessionDocReplica } from '@peri-code/sdk/view';
import { readSyncState } from '../../shared/sync-state';
import type { SyncState } from '../../shared/sync';

export type { SyncState } from '../../shared/sync';
export type SyncStatus = 'connecting' | 'synced' | 'disconnected' | 'failed';
type Socket = Pick<WebSocket, 'onopen' | 'onmessage' | 'onclose' | 'onerror' | 'send' | 'close' | 'binaryType'>;

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
}

export class ChatSync {
  private readonly replica = new SessionDocReplica();
  private socket: Socket | undefined;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private handshake: ReturnType<typeof setTimeout> | undefined;
  private unobserve: (() => void) | undefined;
  private disposed = false;
  private applying = false;
  private attempts = 0;
  private status: SyncStatus = 'connecting';

  constructor(private readonly options: ChatSyncOptions) {
    this.connect();
  }

  private report(status: SyncStatus, error?: Error) {
    this.status = status;
    this.options.onStatus(status, error);
  }

  private publish() {
    const state = readSyncState(this.replica.chat, this.replica.session);
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

  private retireSocket() {
    clearTimeout(this.handshake);
    const socket = this.socket;
    this.socket = undefined;
    if (socket) {
      socket.onopen = socket.onmessage = socket.onclose = socket.onerror = null;
      socket.close();
    }
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
    const url = new URL(`/api/chats/${encodeURIComponent(this.options.chatId)}/sync`,
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
      } catch {
        this.fail(new Error('同步数据格式不正确，请重新加载会话。'));
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
    this.retireSocket();
    this.unobserve?.();
    this.unobserve = undefined;
    this.replica.destroy();
  }
}
