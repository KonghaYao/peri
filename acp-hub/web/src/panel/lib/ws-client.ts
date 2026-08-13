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

import { auth, parse, pong } from './protocol';

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
  onFrame: (frame: Record<string, unknown>) => void;
}

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
    const { url, token, onStatus } = this.opts;
    this.intentional = false;
    this.retryMs = RETRY_BASE_MS;
    onStatus('connecting', {});
    let ws: WebSocket;
    try {
      ws = new WebSocket(url);
    } catch (e) {
      onStatus('fatal', { code: 0, reason: `构造 WebSocket 失败: ${(e as Error).message}` });
      return;
    }
    this.ws = ws;

    ws.onopen = () => {
      // Cookie-authenticated browser clients may subscribe immediately. Legacy
      // clients still send the bearer auth frame as their first application frame.
      try {
        if (token) ws.send(JSON.stringify(auth(token)));
      } catch (e) {
        console.error('[ws-client] auth 发送失败:', e);
        onStatus('fatal', { code: 0, reason: `auth 发送失败: ${(e as Error).message}` });
        return;
      }
      onStatus('open', {});
    };

    ws.onmessage = (ev) => {
      const frame = parse(ev.data as string);
      if (!frame) {
        console.error('[ws-client] 帧解析失败，原始数据:', ev.data);
        return;
      }
      if (frame.t === 'keep_alive') {
        // 心跳：立即回 pong（15s 不回会被 4501 踢掉）。
        this.send(pong());
        onStatus('heartbeat', {});
        return;
      }
      if (frame.t === 'ready') {
        // ready 只在首个订阅时下发一次；本模块上抛，不转发 onFrame
        // （调用方在 onStatus('ready') 里取 projectionVersions）。
        onStatus('ready', frame as ConnDetail);
        return;
      }
      if (frame.t === 'error' || frame.t === 'auth_error' || frame.t === 'action_error') {
        // 错误帧集中打点，便于排查协议/权限问题。
        console.error('[ws-client] 服务端错误帧:', frame);
      }
      this.opts.onFrame(frame);
    };

    ws.onerror = (ev) => {
      console.error('[ws-client] WebSocket 错误:', ev);
    };

    ws.onclose = (ev) => {
      console.warn(`[ws-client] 连接关闭 code=${ev.code} reason=${ev.reason}`);
      this.ws = null;
      if (this.intentional) {
        onStatus('closed', { code: ev.code });
        return;
      }
      if (FATAL_CODES[ev.code]) {
        onStatus('fatal', { code: ev.code, reason: ev.reason || '' });
        return;
      }
      this.scheduleReconnect(ev.code);
    };
  }

  /** 指数退避重连：1s→2s→4s→…≤30s；重连后状态机回 'connecting'，
   *  订阅重放由调用方在收到 'open' 时执行（快照兜底，M3 §4 流程 6）。 */
  private scheduleReconnect(code: number): void {
    this.opts.onStatus('reconnecting', { retryMs: this.retryMs, code });
    this.retryTimer = setTimeout(() => {
      this.connect();
    }, this.retryMs);
    this.retryMs = Math.min(this.retryMs * 2, RETRY_MAX_MS);
  }

  /** 发送对象帧（自动序列化）；连接未就绪时丢弃并返回 false。 */
  send(frame: unknown): boolean {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(frame));
      return true;
    }
    return false;
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
