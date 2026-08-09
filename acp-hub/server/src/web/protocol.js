// acp-hub Web 面板 —— 帧协议模块。
//
// 与 server 的 client 面协议逐字对齐（M3 方案 §1）：
//   - payload 字段 camelCase；`t` 为顶层判别字段；
//   - `action_ack.status` 为小写 snake_case（accepted/committed/duplicate）；
//   - `action_error.code` 为 SCREAMING_SNAKE_CASE；
//   - ysync 订阅字段名是 `docs`（DocId 透明字符串）；
//   - 快照帧带 `projectionVersion`，增量帧不带（serde skip，不输出 null）。
//
// 本模块只负责帧的构造/解析与 base64 工具，不持有任何连接状态
// （连接状态机见 ws-client.js）。

(function () {
  'use strict';

  // 注册表 doc id（与 proto/src/conn.rs 的 DocId::REGISTRY 对齐），常驻订阅。
  var DOC_REGISTRY = 'hub:registry';

  // chat 派生 doc id（订阅字段透明字符串，前缀区分投影）。
  function chatDoc(sid) {
    return 'chat:' + sid;
  }
  function controlDoc(sid) {
    return 'control:' + sid;
  }

  // action 幂等键：crypto.randomUUID（浏览器全局；localhost/https 下可用）。
  function newCommandId() {
    return crypto.randomUUID();
  }

  // ── 上行帧 ──────────────────────────────────────────────────────────────

  // 认证帧：ws 握手后第一帧必须是它（否则 server 以 1011 关闭）。client 面
  // 无 HMAC（§9.2 仅覆盖 instance 连接），token 为 44 字符 base64，顶层字段。
  // 认证失败 → server 以 4502 关闭。
  function auth(token) {
    return { t: 'auth', token: token };
  }

  // ysync 订阅：字段名 `docs`；首个订阅后 server 推各 doc 快照 + ready。
  function subscribe(docs) {
    return { t: 'ysync.subscribe', docs: docs };
  }

  // ysync 退订：幂等，重复退订无副作用。
  function unsubscribe(docs) {
    return { t: 'ysync.unsubscribe', docs: docs };
  }

  // 心跳应答：server 每 5s 发 keep_alive，15s（3×间隔）内不回 → 4501 关闭。
  function pong() {
    return { t: 'pong' };
  }

  // ── action 帧 ───────────────────────────────────────────────────────────

  // 外层包装：t/commandId/type/payload 四字段；commandId 为幂等键
  // （重试复用同一 commandId，server 幂等去重 → duplicate ack）。
  function action(type, payload) {
    return {
      t: 'action',
      commandId: newCommandId(),
      type: type,
      payload: payload,
    };
  }

  // 新建对话：三字段全可选（缺省 = 本机）。committed ack 携带的 chatId
  // 是 server 生成 id 的唯一告知路径 → main.js 据此自动补订阅并选中。
  function createChat(title) {
    var payload = {};
    if (title) payload.title = title;
    return action('chat/create', payload);
  }

  // 发送消息（两阶段 ack：accepted → committed）。
  function prompt(chatId, message) {
    return action('chat/prompt', { chatId: chatId, message: message });
  }

  // 取消当前 turn。
  function cancel(chatId) {
    return action('chat/cancel', { chatId: chatId });
  }

  // 关闭对话。
  function close(chatId) {
    return action('chat/close', { chatId: chatId });
  }

  // 权限裁决：decision 仅 "allow" | "deny"。
  function resolvePermission(chatId, permissionId, decision) {
    return action('permission/resolve', {
      chatId: chatId,
      permissionId: permissionId,
      decision: decision,
    });
  }

  // ── 下行解析 ────────────────────────────────────────────────────────────

  // 解析下行文本帧为对象；非 JSON → null（调用方记日志即可，不抛异常）。
  function parse(text) {
    try {
      return JSON.parse(text);
    } catch (e) {
      return null;
    }
  }

  // ── base64 ↔ Uint8Array（浏览器内置 atob/btoa）──────────────────────────

  // base64 → Uint8Array：yrs v1 update 载荷（快照/增量同格式）。
  function base64ToBytes(b64) {
    var bin = atob(b64);
    var bytes = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) {
      bytes[i] = bin.charCodeAt(i);
    }
    return bytes;
  }

  // Uint8Array → base64（当前仅调试输出用）。
  function bytesToBase64(bytes) {
    var bin = '';
    for (var i = 0; i < bytes.length; i++) {
      bin += String.fromCharCode(bytes[i]);
    }
    return btoa(bin);
  }

  // 导出全局单例（panel.html 中 protocol.js 先于其他脚本加载）。
  window.HubProtocol = {
    DOC_REGISTRY: DOC_REGISTRY,
    chatDoc: chatDoc,
    controlDoc: controlDoc,
    newCommandId: newCommandId,
    auth: auth,
    subscribe: subscribe,
    unsubscribe: unsubscribe,
    pong: pong,
    createChat: createChat,
    prompt: prompt,
    cancel: cancel,
    close: close,
    resolvePermission: resolvePermission,
    parse: parse,
    base64ToBytes: base64ToBytes,
    bytesToBase64: bytesToBase64,
  };
})();
