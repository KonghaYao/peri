import { frameBytes, MAX_SYNC_FRAME_BYTES, type ChatYjsChannel } from "@peri-code/sdk/view";
import type { ChatProjection } from "./projection";

/** 连接状态只描述物理通道；副本 readiness/首次同步由投影另行报告。 */
export type SyncConnectionStatus = "connecting" | "open" | "closed" | "failed";

/** 慢客户端显式失败：不静默丢弃 HELLO/ACK，也不假装连接仍可用。 */
export class SyncBackpressureError extends Error {
  constructor(readonly bufferedBytes: number) {
    super(`Chat sync socket backpressure (${bufferedBytes} bytes buffered)`);
    this.name = "SyncBackpressureError";
  }
}

/** 客户端单帧发送预算：只会发送 HELLO/ACK。 */
const MAX_PENDING_SEND_FRAMES = 8;
const MAX_BUFFERED_BYTES = 1_048_576;
const NORMAL_CLOSE = 1000;

export interface WebSocketLike {
  binaryType: string;
  readonly readyState: number;
  readonly bufferedAmount: number;
  onopen: (() => void) | null;
  onmessage: ((event: { data: unknown }) => void) | null;
  onerror: ((event: unknown) => void) | null;
  onclose: ((event: { code: number; reason: string }) => void) | null;
  send(data: Uint8Array): void;
  close(code?: number, reason?: string): void;
}

const OPEN = 1;
const CLOSED = 3;

/**
 * 浏览器 WebSocket → SDK `ChatYjsChannel`。
 *
 * 只传输二进制帧；文本、超限和背压都显式关闭。连接打开前的 HELLO 有界排队，
 * 打开后按序发送，不阻塞 SDK 的 ACK。
 */
export class WebSocketChatChannel implements ChatYjsChannel {
  private socket: WebSocketLike | undefined;
  private state = false;
  private readonly pending: Uint8Array[] = [];
  private readonly messageListeners = new Set<(frame: Uint8Array) => void>();
  private readonly closeListeners = new Set<(error?: Error) => void>();
  private failure?: Error;

  constructor(
    readonly url: string,
    factory: (url: string) => WebSocketLike = (target) => new WebSocket(target) as unknown as WebSocketLike,
  ) {
    const socket = factory(url);
    socket.binaryType = "arraybuffer";
    this.socket = socket;
    socket.onopen = () => this.open();
    socket.onmessage = (event) => void this.receive(event.data);
    socket.onerror = () => this.fail(new Error("Chat sync socket error"));
    socket.onclose = (event) => this.peerClosed(event.code, event.reason);
  }

  get closed(): boolean { return this.state; }

  send(frame: Uint8Array): void {
    if (this.state) throw this.failure ?? new Error("Chat sync channel is closed");
    const socket = this.socket;
    if (!socket) throw new Error("Chat sync channel has no socket");
    if (frame.byteLength > MAX_SYNC_FRAME_BYTES) throw new Error("Chat sync frame exceeds byte budget");
    if (socket.readyState !== OPEN) {
      if (this.pending.length >= MAX_PENDING_SEND_FRAMES)
        throw new SyncBackpressureError(socket.bufferedAmount);
      this.pending.push(frame);
      return;
    }
    this.write(socket, frame);
  }

  onMessage(listener: (frame: Uint8Array) => void): () => void {
    if (this.state) return () => {};
    this.messageListeners.add(listener);
    return () => this.messageListeners.delete(listener);
  }

  onClose(listener: (error?: Error) => void): () => void {
    if (this.state) {
      listener(this.failure ?? new Error("Chat sync channel is already closed"));
      return () => {};
    }
    this.closeListeners.add(listener);
    return () => this.closeListeners.delete(listener);
  }

  close(reason?: Error): void {
    if (this.state) return;
    this.finish(reason);
    const socket = this.socket;
    this.socket = undefined;
    if (socket && socket.readyState !== CLOSED) {
      socket.close(reason ? 1011 : NORMAL_CLOSE, reason?.message.slice(0, 120));
    }
  }

  private open(): void {
    if (this.state) return;
    const socket = this.socket;
    if (!socket) return;
    for (const frame of this.pending.splice(0)) {
      try { this.write(socket, frame); }
      catch (error) { this.fail(error instanceof Error ? error : new Error("Chat sync send failed")); return; }
    }
  }

  private write(socket: WebSocketLike, frame: Uint8Array): void {
    if (socket.bufferedAmount > MAX_BUFFERED_BYTES) {
      const error = new SyncBackpressureError(socket.bufferedAmount);
      this.fail(error);
      throw error;
    }
    socket.send(frame);
  }

  private async receive(raw: unknown): Promise<void> {
    if (this.state) return;
    const bytes = frameBytes(raw instanceof Blob ? new Uint8Array(await raw.arrayBuffer()) : raw);
    if (!bytes) {
      this.fail(new Error("Chat sync requires binary frames"));
      return;
    }
    if (bytes.byteLength > MAX_SYNC_FRAME_BYTES) {
      this.fail(new Error("Chat sync frame exceeds receive budget"));
      return;
    }
    for (const listener of [...this.messageListeners]) listener(bytes);
  }

  private peerClosed(code: number, reason: string): void {
    if (this.state) return;
    const detail = reason ? `: ${reason}` : "";
    this.finish(new Error(`Chat sync socket closed (${code}${detail})`));
  }

  private fail(error: Error): void {
    if (this.state) return;
    this.finish(error);
    const socket = this.socket;
    this.socket = undefined;
    if (socket && socket.readyState !== CLOSED) socket.close(1011, error.message.slice(0, 120));
  }

  private finish(error?: Error): void {
    this.state = true;
    this.failure = error;
    this.pending.length = 0;
    for (const listener of [...this.closeListeners]) listener(error);
    this.closeListeners.clear();
    this.messageListeners.clear();
  }
}

export interface ChatSyncOptions {
  url: string;
  projection: ChatProjection;
  onStatus?: (status: SyncConnectionStatus, error?: Error) => void;
  /** 测试注入；默认浏览器 WebSocket。 */
  webSocketFactory?: (url: string) => WebSocketLike;
  /** 断线自动重连；默认开启（1s 起，最多 15s）。 */
  reconnect?: boolean;
  reconnectDelayMs?: number;
  maxReconnectDelayMs?: number;
  maxAttempts?: number;
}

/**
 * 每 Chat 一条只读同步连接，并在断线后重连。
 *
 * 重连使用同一 `ChatReplica`：HELLO 携带既有 state vector，服务端据此发 delta
 * 或成对 SNAPSHOT。慢客户端/协议帧错误显式结束，不做无界重试。
 */
export class ChatSyncProvider {
  private channel: WebSocketChatChannel | undefined;
  private reverse: { close(): Promise<void> } | undefined;
  private disposed = false;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private attempt = 0;
  private current: SyncConnectionStatus = "connecting";
  private error: Error | undefined;
  private readonly unsubscribe: () => void;

  constructor(private readonly options: ChatSyncOptions) {
    this.unsubscribe = options.projection.subscribe(() => {
      if (this.disposed || !this.channel) return;
      if (options.projection.status === "synced") {
        // 成功同步即清零重连预算：连续失败才消耗预算，长连接寿命内不被历史断线拖垮。
        this.attempt = 0;
        if (this.current !== "open") this.report("open");
      }
    });
  }

  get status(): SyncConnectionStatus { return this.current; }
  get lastError(): Error | undefined { return this.error; }

  start(): void {
    if (this.disposed || this.channel) return;
    this.open();
  }

  async close(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    this.unsubscribe();
    if (this.timer !== undefined) { clearTimeout(this.timer); this.timer = undefined; }
    const reverse = this.reverse;
    this.reverse = undefined;
    await reverse?.close().catch(() => {});
    this.channel?.close();
    this.channel = undefined;
    this.report("closed");
  }

  private open(): void {
    const channel = new WebSocketChatChannel(this.options.url, this.options.webSocketFactory);
    this.channel = channel;
    this.report("connecting");
    try {
      this.reverse = this.options.projection.connect(channel);
    } catch (error) {
      this.fail(error instanceof Error ? error : new Error("Chat sync connect failed"));
      return;
    }
    channel.onClose((error) => {
      if (this.channel !== channel) return;
      void this.reverse?.close().catch(() => {});
      this.reverse = undefined;
      this.channel = undefined;
      if (this.disposed) return;
      if (error) {
        this.fail(error);
        return;
      }
      this.report("closed");
    });
  }

  private fail(error: Error): void {
    this.error = error;
    this.report("failed", error);
    if (this.disposed || error instanceof SyncBackpressureError) return;
    const reconnect = this.options.reconnect ?? true;
    if (!reconnect) return;
    const maxAttempts = this.options.maxAttempts ?? 5;
    if (this.attempt >= maxAttempts) return;
    const initial = this.options.reconnectDelayMs ?? 1_000;
    const maximum = this.options.maxReconnectDelayMs ?? 15_000;
    const delay = Math.min(maximum, initial * 2 ** this.attempt);
    this.attempt += 1;
    // 保留 failed，直到下一次连接真正开始；避免把重连等待伪装成「连接中」。
    this.timer = setTimeout(() => {
      this.timer = undefined;
      if (this.disposed) return;
      this.open();
    }, delay);
  }

  private report(status: SyncConnectionStatus, error?: Error): void {
    this.current = status;
    if (error) this.error = error;
    this.options.onStatus?.(status, error);
  }
}
