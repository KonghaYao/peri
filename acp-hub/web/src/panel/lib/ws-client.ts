// acp-hub Web 面板 —— ws 客户端模块（移植自原 ws-client.js，TS 化）。
//
// 职责（M3 方案 §1/§4）：
//   - 连接状态机：connecting → open → ready（server 首个订阅
//     后下发）；ready 帧同时上抛给调用方（onStatus 带完整帧）。
//   - 浏览器使用握手 Cookie；可选 token 仅供旧 wire-auth client 兼容。
//   - 心跳：收到 keep_alive 立即回 pong（server 每 5s 一次，15s 不回 → 4501）。
//   - 关闭码 → 重连策略：4500/4501/4502 永久停止（实例离线/心跳超时/认证
//     失败），其余（1011 通用、1013 配额、1006 网络异常等）指数退避重连
//     （1s→2s→4s→…≤30s）。重连后由调用方重放订阅（快照兜底）。
//
// 回调接口：
//   onStatus(state, detail)
//     state: 'connecting' | 'open' | 'ready' | 'heartbeat' |
//            'reconnecting' | 'fatal' | 'closed'
//   onFrame(frame)   —— 每个非 keep_alive 下行帧（已 parse 的对象）。

import { auth, parse, pong, type DownstreamFrame } from './protocol';

export type ConnStatus =
  | 'connecting'
  | 'open'
  | 'ready'
  | 'heartbeat'
  | 'reconnecting'
  | 'fatal'
  | 'closed';

export interface ConnDetail {
  code?: number;
  reason?: string;
  retryMs?: number;
  /** ready 帧完整对象（projectionVersions 等）。 */
  [key: string]: unknown;
}

export interface WsClientOpts {
  url: string;
  token?: string;
  onStatus: (state: ConnStatus, detail: ConnDetail) => void;
  onFrame: (frame: DownstreamFrame) => void;
  onProtocolIssue?: (issue: WsProtocolIssue) => void;
}

export type WsProtocolIssue =
  | { kind: 'non_text_frame'; size: number }
  | { kind: 'malformed_frame'; size: number }
  | { kind: 'callback_error'; callback: 'status' | 'frame' }
  | { kind: 'send_error' };

// 退避参数：1s 起步，倍增封顶 30s。
const RETRY_BASE_MS = 1000;
const RETRY_MAX_MS = 30000;

// 永久失败关闭码（M3 方案 §1.2）：不自动重连。
const FATAL_CODES: Record<number, boolean> = { 4500: true, 4501: true, 4502: true };

export class WsClient {
  private ws: WebSocket | null = null;
  private retryMs = RETRY_BASE_MS;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private intentional = false; // 用户主动 close → 不再重连
  private readonly opts: WsClientOpts;

  constructor(opts: WsClientOpts) {
    this.opts = opts;
  }

  /** 建立（或重建立）连接。调用方每次 connect 前应确保旧实例已 close。 */
  connect(): void {
    this.openConnection(true);
  }

  private openConnection(resetBackoff: boolean): void {
    const { url, token } = this.opts;
    this.intentional = false;
    if (resetBackoff) this.retryMs = RETRY_BASE_MS;
    this.emitStatus('connecting', {});
    let ws: WebSocket;
    try {
      ws = new WebSocket(url);
    } catch (e) {
      this.emitStatus('fatal', { code: 0, reason: `构造 WebSocket 失败: ${(e as Error).message}` });
      return;
    }
    this.ws = ws;

    ws.onopen = () => {
      // Cookie-authenticated browser clients may subscribe immediately. Legacy
      // clients still send the bearer auth frame as their first application frame.
      try {
        if (token) ws.send(JSON.stringify(auth(token)));
      } catch {
        // Bearer token is part of this frame. Never attach the thrown value: a
        // browser/polyfill may include serialized arguments in its error text.
        console.error('[ws-client] auth 发送失败');
        this.emitStatus('fatal', { code: 0, reason: 'auth 发送失败' });
        return;
      }
      this.emitStatus('open', {});
    };

    ws.onmessage = (ev) => {
      if (typeof ev.data !== 'string') {
        const size = binarySize(ev.data);
        console.error(`[ws-client] 忽略非文本帧 length=${size}`);
        this.emitIssue({ kind: 'non_text_frame', size });
        return;
      }
      const frame = parse(ev.data);
      if (!frame) {
        const size = ev.data.length;
        console.error(`[ws-client] 帧解析失败 length=${size}`);
        this.emitIssue({ kind: 'malformed_frame', size });
        return;
      }
      if (frame.t === 'keep_alive') {
        // 心跳：立即回 pong（15s 不回会被 4501 踢掉）。
        if (!this.send(pong())) return;
        this.emitStatus('heartbeat', {});
        return;
      }
      if (frame.t === 'ready') {
        // ready 只在首个订阅时下发一次；本模块上抛，不转发 onFrame
        // （调用方在 onStatus('ready') 里取 projectionVersions）。
        this.retryMs = RETRY_BASE_MS;
        this.emitStatus('ready', frame as ConnDetail);
        return;
      }
      if (frame.t === 'error' || frame.t === 'auth_error' || frame.t === 'action_error') {
        // 只记录协议类型；message/details 可含工具参数或用户内容。
        console.error(`[ws-client] 服务端错误帧 type=${frame.t}`);
      }
      try {
        this.opts.onFrame(frame);
      } catch {
        console.error(`[ws-client] 下行帧处理失败 type=${frame.t}`);
        this.emitIssue({ kind: 'callback_error', callback: 'frame' });
      }
    };

    ws.onerror = () => {
      console.error('[ws-client] WebSocket 传输错误');
    };

    ws.onclose = (ev) => {
      console.warn(`[ws-client] 连接关闭 code=${ev.code}`);
      this.ws = null;
      if (this.intentional) {
        this.emitStatus('closed', { code: ev.code });
        return;
      }
      if (FATAL_CODES[ev.code]) {
        this.emitStatus('fatal', { code: ev.code, reason: ev.reason || '' });
        return;
      }
      this.scheduleReconnect(ev.code);
    };
  }

  /** 指数退避重连：1s→2s→4s→…≤30s；重连后状态机回 'connecting'，
   *  订阅重放由调用方在收到 'open' 时执行（快照兜底，M3 §4 流程 6）。 */
  private scheduleReconnect(code: number): void {
    this.emitStatus('reconnecting', { retryMs: this.retryMs, code });
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      this.openConnection(false);
    }, this.retryMs);
    this.retryMs = Math.min(this.retryMs * 2, RETRY_MAX_MS);
  }

  /** 发送对象帧（自动序列化）；连接未就绪时丢弃并返回 false。 */
  send(frame: unknown): boolean {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      try {
        this.ws.send(JSON.stringify(frame));
        return true;
      } catch {
        console.error('[ws-client] 上行帧发送失败');
        this.emitIssue({ kind: 'send_error' });
      }
    }
    return false;
  }

  private emitStatus(state: ConnStatus, detail: ConnDetail): void {
    try {
      this.opts.onStatus(state, detail);
    } catch {
      console.error(`[ws-client] 状态回调失败 state=${state}`);
      this.emitIssue({ kind: 'callback_error', callback: 'status' });
    }
  }

  private emitIssue(issue: WsProtocolIssue): void {
    try {
      this.opts.onProtocolIssue?.(issue);
    } catch {
      console.error(`[ws-client] 协议异常回调失败 kind=${issue.kind}`);
    }
  }

  /** 用户主动断开：停止一切重连，连接关闭后回调 'closed'。 */
  close(): void {
    if (this.retryTimer) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
    this.intentional = true;
    if (this.ws) {
      this.ws.close();
    }
  }
}

function binarySize(data: unknown): number {
  if (data instanceof Blob) return data.size;
  if (data instanceof ArrayBuffer) return data.byteLength;
  if (ArrayBuffer.isView(data)) return data.byteLength;
  return 0;
}
