// acp-hub Web 面板 —— ws 客户端模块。
//
// 职责（M3 方案 §1/§4）：
//   - 连接状态机：connecting → open（auth 已发）→ ready（server 首个订阅
//     后下发）；ready 帧同时上抛给调用方（onStatus detail = 完整帧）。
//   - 首帧纪律：ws 握手后第一帧必须是 auth，由本模块在 onopen 内强制发送
//     （否则 server 1011 关闭，gateway.rs §4.6）。
//   - 心跳：收到 keep_alive 立即回 pong（server 每 5s 一次，15s 不回 → 4501）。
//   - 关闭码 → 重连策略：4500/4501/4502 永久停止（实例离线/心跳超时/认证
//     失败），其余（1011 通用、1013 配额、1006 网络异常等）指数退避重连
//     （1s→2s→4s→…≤30s）。重连后由 main.js 重放订阅（快照兜底）。
//
// 回调接口：
//   onStatus(state, detail)
//     state: 'connecting' | 'open' | 'ready' | 'heartbeat' |
//            'reconnecting' | 'fatal' | 'closed'
//   onFrame(frame)   —— 每个非 keep_alive 下行帧（已 parse 的对象）。

(function () {
  'use strict';

  // 退避参数：1s 起步，倍增封顶 30s。
  var RETRY_BASE_MS = 1000;
  var RETRY_MAX_MS = 30000;

  // 永久失败关闭码（M3 方案 §1.2）：不自动重连。
  var FATAL_CODES = { 4500: true, 4501: true, 4502: true };

  function WsClient(opts) {
    this.url = opts.url;
    this.token = opts.token;
    this.onStatus = opts.onStatus || function () {};
    this.onFrame = opts.onFrame || function () {};

    this.ws = null;
    this.retryMs = RETRY_BASE_MS;
    this.retryTimer = null;
    this.intentional = false; // 用户主动 close → 不再重连
  }

  // 建立（或重建立）连接。调用方每次 connect 前应确保旧实例已 close。
  WsClient.prototype.connect = function () {
    var self = this;
    this.intentional = false;
    this.retryMs = RETRY_BASE_MS;
    this.onStatus('connecting', {});
    var ws;
    try {
      ws = new WebSocket(this.url);
    } catch (e) {
      this.onStatus('fatal', { code: 0, reason: '构造 WebSocket 失败: ' + e.message });
      return;
    }
    this.ws = ws;

    ws.onopen = function () {
      // 首帧纪律：auth 必须是第一帧（无 HMAC，client 面单向认证）。
      try {
        ws.send(JSON.stringify(window.HubProtocol.auth(self.token)));
      } catch (e) {
        console.error('[ws-client] auth 发送失败:', e);
        self.onStatus('fatal', { code: 0, reason: 'auth 发送失败: ' + e.message });
        return;
      }
      self.onStatus('open', {});
    };

    ws.onmessage = function (ev) {
      var frame = window.HubProtocol.parse(ev.data);
      if (!frame) {
        console.error('[ws-client] 帧解析失败，原始数据:', ev.data);
        return;
      }
      if (frame.t === 'keep_alive') {
        // 心跳：立即回 pong（15s 不回会被 4501 踢掉）。
        self.send(window.HubProtocol.pong());
        self.onStatus('heartbeat', {});
        return;
      }
      if (frame.t === 'ready') {
        // ready 只在首个订阅时下发一次；本模块上抛，不转发 onFrame
        // （调用方在 onStatus('ready') 里取 projectionVersions）。
        self.onStatus('ready', frame);
        return;
      }
      if (frame.t === 'error' || frame.t === 'auth_error' || frame.t === 'action_error') {
        // 错误帧集中打点，便于排查协议/权限问题。
        console.error('[ws-client] 服务端错误帧:', frame);
      }
      self.onFrame(frame);
    };

    ws.onerror = function (ev) {
      console.error('[ws-client] WebSocket 错误:', ev && ev.message ? ev.message : ev);
    };

    ws.onclose = function (ev) {
      console.warn('[ws-client] 连接关闭 code=' + ev.code + ' reason=' + ev.reason);
      self.ws = null;
      if (self.intentional) {
        self.onStatus('closed', { code: ev.code });
        return;
      }
      if (FATAL_CODES[ev.code]) {
        self.onStatus('fatal', { code: ev.code, reason: ev.reason || '' });
        return;
      }
      self.scheduleReconnect(ev.code);
    };
  };

  // 指数退避重连：1s→2s→4s→…≤30s；重连后状态机回 'connecting'，
  // 订阅重放由调用方在收到 'open' 时执行（快照兜底，M3 §4 流程 6）。
  WsClient.prototype.scheduleReconnect = function (code) {
    var self = this;
    this.onStatus('reconnecting', { retryMs: this.retryMs, code: code });
    this.retryTimer = setTimeout(function () {
      self.connect();
    }, this.retryMs);
    this.retryMs = Math.min(this.retryMs * 2, RETRY_MAX_MS);
  };

  // 发送对象帧（自动序列化）；连接未就绪时丢弃并返回 false。
  WsClient.prototype.send = function (frame) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(frame));
      return true;
    }
    return false;
  };

  // 用户主动断开：停止一切重连，连接关闭后回调 'closed'。
  WsClient.prototype.close = function () {
    this.intentional = true;
    if (this.retryTimer) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  };

  window.WsClient = WsClient;
})();
