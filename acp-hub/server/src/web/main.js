// acp-hub Web 面板 —— 装配与编排。
//
// 流程（M3 方案 §4）：
//   1. 连接 → auth（ws-client 首帧纪律）→ ysync.subscribe ["hub:registry"]
//      → 快照 + ready → UI 启用。
//   2. registry 渲染 → 左栏实例/对话。
//   3. 点击对话 → subscribe ["chat:{cid}","control:{cid}"] → 快照渲染历史
//      → 增量实时更新（yjs 流式）。
//   4. 发送消息 → chat/prompt（commandId 记入 pendingAcks）→ 输入框清空。
//      用户消息气泡依赖 agent 回显（server 单写者，本地不造假）。
//   5. create committed ack（带 chatId）→ 自动订阅 chat:{cid} 并选中。
//   6. 断线：4500/4501/4502 停止并提示；1011/1013 指数退避重连（ws-client），
//      重连后重放订阅（快照兜底）。
//
// token 解析优先级（M3 §4）：URL ?token= → sessionStorage（避免落盘明文）
// → 输入框粘贴。

(function () {
  'use strict';

  var H = window.HubProtocol;
  var YV = window.YjsView;
  var UI = new window.PanelUI();

  // 若无 yjs（CDN 离线），面板无法渲染，直接停在这里（横幅已由
  // panel.html 内联脚本显示）。
  if (typeof Y === 'undefined') {
    UI.toast('Yjs 未加载，面板不可用（详见顶部横幅）');
    return;
  }

  var TOKEN_KEY = 'acp-hub-token';
  var ACK_TIMEOUT_MS = 30000;

  var store = new YV.DocStore(); // docId → Y.Doc
  var ws = null;                 // 当前 WsClient
  var currentCid = null;         // 选中对话（重连后恢复订阅）
  var chatStatus = {};        // cid → registry 状态（终态禁用交互）
  var ready = false;             // ready 门控：就绪后才发 action
  var pendingAcks = new Map();   // commandId → {label, cb, timer}

  // ── token 解析 ──────────────────────────────────────────────────────────

  function resolveToken() {
    var params = new URLSearchParams(window.location.search);
    var fromUrl = params.get('token');
    if (fromUrl) {
      // base64 token 含 `+`，URL query 中 `+` 被解码为空格——还原之。
      fromUrl = fromUrl.replace(/ /g, '+');
      // URL token 是一次性入口：写入 sessionStorage 供刷新复用，随后从
      // URL 中清理（避免明文留在地址栏/历史）。
      sessionStorage.setItem(TOKEN_KEY, fromUrl);
      params.delete('token');
      var qs = params.toString();
      var next = window.location.pathname + (qs ? '?' + qs : '');
      window.history.replaceState(null, '', next);
      return fromUrl;
    }
    return sessionStorage.getItem(TOKEN_KEY) || '';
  }

  // ── 订阅集合：registry 常驻 + 当前对话（chat + control 双 doc）──────────

  function desiredDocs() {
    var docs = [H.DOC_REGISTRY];
    if (currentCid) {
      docs.push(H.chatDoc(currentCid));
      docs.push(H.controlDoc(currentCid));
    }
    return docs;
  }

  // 订阅重放（首次连接与断线重连共用；ysync.subscribe 幂等）。
  function sendSubscribe() {
    if (!ws || !ws.send(H.subscribe(desiredDocs()))) {
      UI.toast('连接未就绪，订阅失败');
    }
  }

  // ── 对话选择 ────────────────────────────────────────────────────────────

  // 终态对话（ACP 进程已退出/用户关闭/崩溃）：只读历史，禁交互。
  function isTerminal(status) {
    return status === 'ended' || status === 'closed' || status === 'crashed';
  }

  function selectChat(cid) {
    currentCid = cid;
    sendSubscribe(); // 幂等；快照到达后由 onUpdate 渲染
    UI.setCurrentChat(cid, chatStatus[cid] || '');
    UI.setReady(null); // 订阅清单等下一帧 ready 刷新（简单置空亦可）
    UI.renderPermissions([]);
  }

  // ── ack 表 ──────────────────────────────────────────────────────────────

  // 发送 action 并登记 ack 回调；ready 前不发（server 会缓冲，但面板
  // 以 ready 门控保证可预期）。
  function sendAction(frame, label, cb) {
    if (!ws || !ws.send(frame)) {
      UI.toast('连接未就绪，无法发送 ' + label);
      return;
    }
    var timer = setTimeout(function () {
      pendingAcks.delete(frame.commandId);
      UI.toast('ack 超时（30s）: ' + label + ' ' + frame.commandId.slice(0, 8) + '…');
    }, ACK_TIMEOUT_MS);
    pendingAcks.set(frame.commandId, { label: label, cb: cb, timer: timer });
  }

  // ── 下行帧分发 ──────────────────────────────────────────────────────────

  function onFrame(frame) {
    switch (frame.t) {
      case 'ysync.update':
        store.applyUpdateFrame(frame);
        break;
      case 'action_ack':
        onAck(frame);
        break;
      case 'action_error':
        onActionError(frame);
        break;
      default:
        break; // 未知帧忽略（协议演进兼容）
    }
  }

  function onAck(ack) {
    UI.showAck(ack);
    var pending = pendingAcks.get(ack.commandId);
    if (pending) {
      clearTimeout(pending.timer);
      pendingAcks.delete(ack.commandId);
      if (typeof pending.cb === 'function') pending.cb(ack);
    }
    // create 的 committed ack 携带 server 生成的 chatId —— 唯一告知
    // 路径：自动补订阅 chat:{cid}/control:{cid} 并选中。
    if (ack.status === 'committed' && ack.chatId && ack.chatId !== currentCid) {
      UI.toast('对话已创建: ' + ack.chatId.slice(0, 8) + '…');
      selectChat(ack.chatId);
    }
  }

  function onActionError(err) {
    console.error('[panel] action 错误:', JSON.stringify(err));
    UI.showActionError(err);
    var pending = pendingAcks.get(err.commandId);
    if (pending) {
      clearTimeout(pending.timer);
      pendingAcks.delete(err.commandId);
    }
    var reason = {
      CHAT_NOT_FOUND: '对话不存在或已关闭',
      INSTANCE_OFFLINE: '实例离线',
      FORBIDDEN: '无权限（token 角色不足？）',
      VERSION_CONFLICT: '版本冲突',
      RATE_LIMITED: '限流',
      AGENT_UNAVAILABLE: 'agent 不可用',
      PAYLOAD_TOO_LARGE: '载荷过大',
      UNSUPPORTED_FRAME: '不支持的操作',
      UNAUTHENTICATED: '未认证',
      INVALID_STATE: '非法状态',
    }[err.code];
    UI.toast((err.code || 'ACTION_ERROR') + (reason ? '：' + reason : '') +
      (err.message ? '（' + err.message + '）' : ''));
  }

  // ── 渲染入口（store.onUpdate：rAF 合帧后每个被更新 doc 调一次）──────────

  store.onUpdate = function (docId) {
    if (docId === H.DOC_REGISTRY) {
      var reg = YV.renderRegistry(store.docFor(docId));
      // 状态映射（终态判定）：selectChat 需要当前 status。
      chatStatus = {};
      reg.chats.forEach(function (s) { chatStatus[s.id] = s.status; });
      UI.renderInstances(reg.instances);
      UI.renderChats(reg.chats, currentCid);
      UI.setGlobalStatus(reg.globalStatus);
      if (currentCid && currentCid in chatStatus) {
        UI.setCurrentChat(currentCid, chatStatus[currentCid]);
      }
      return;
    }
    if (currentCid && docId === H.chatDoc(currentCid)) {
      UI.renderChat(YV.renderChat(store.docFor(docId)));
      return;
    }
    if (currentCid && docId === H.controlDoc(currentCid)) {
      var sess = YV.renderControl(store.docFor(docId));
      UI.renderChatHead(sess);
      UI.renderPermissions(sess.pendingPermissions);
    }
  };

  // ── 连接状态机回调（ws-client）─────────────────────────────────────────

  function onStatus(state, detail) {
    switch (state) {
      case 'connecting':
        ready = false;
        UI.setConnState('连接中…', '');
        break;
      case 'open':
        // 已发 auth；认证后首帧必须是 ysync.subscribe 或 action ——
        // 立即重放订阅（首次连接与重连同一路径，快照兜底）。
        ready = false;
        UI.setConnState('已认证', 'ok');
        sendSubscribe();
        break;
      case 'ready':
        ready = true;
        UI.setConnState('就绪', 'ok');
        UI.setReady(detail.projectionVersions);
        UI.toast('连接就绪');
        break;
      case 'heartbeat':
        UI.bumpHeartbeat();
        break;
      case 'reconnecting':
        UI.setConnState('重连中（' + Math.round(detail.retryMs / 1000) + 's 后）', 'warn');
        break;
      case 'fatal':
        ready = false;
        // connect() 置 busy(true) 后无任何路径恢复（closed 仅由用户主动
        // disconnect 触发，那里已 setBusy(false)）→ 必须在此恢复按钮，
        // 否则 4500/4501/4502 后 connect/disconnect 双双 disabled 无法重连。
        UI.setBusy(false);
        UI.setConnState('已停止（' + detail.code + '）', 'err');
        UI.toast(
          '连接终止（' + detail.code + '）：' +
          (detail.code === 4500 ? '实例离线' :
           detail.code === 4501 ? '心跳超时' :
           detail.code === 4502 ? '认证失败/配置性失败' : '未知原因') +
          '，不自动重连'
        );
        break;
      case 'closed':
        ready = false;
        UI.setConnState('已断开', '');
        break;
      default:
        break;
    }
  }

  function wsUrl() {
    var scheme = window.location.protocol === 'https:' ? 'wss://' : 'ws://';
    return scheme + window.location.host + '/';
  }

  function connect(token) {
    if (!token) {
      UI.toast('请先粘贴 token（或 ?token= 传入）');
      return;
    }
    sessionStorage.setItem(TOKEN_KEY, token);
    if (ws) {
      ws.close();
      ws = null;
    }
    pendingAcks.forEach(function (p) {
      clearTimeout(p.timer);
    });
    pendingAcks.clear();
    ws = new window.WsClient({
      url: wsUrl(),
      token: token,
      onStatus: onStatus,
      onFrame: onFrame,
    });
    ws.connect();
    UI.setBusy(true);
  }

  function disconnect() {
    if (ws) {
      ws.close();
      ws = null;
    }
    pendingAcks.forEach(function (p) {
      clearTimeout(p.timer);
    });
    pendingAcks.clear();
    UI.setBusy(false);
  }

  // ── 用户动作 → action ──────────────────────────────────────────────────

  function sendMessage(text) {
    if (!ready) {
      UI.toast('连接未就绪，稍后再试');
      return;
    }
    if (!currentCid) {
      UI.toast('请先选择对话');
      return;
    }
    if (isTerminal(chatStatus[currentCid])) {
      UI.toast('对话已结束，不能发送消息');
      return;
    }
    // 输入框已由 UI 清空；用户消息气泡依赖 agent 回显（server 单写者，
    // 本地不造假，保证多标签页一致性）。
    sendAction(H.prompt(currentCid, text), 'prompt', function (ack) {
      if (ack.status === 'committed' && ack.turnId) {
        UI.toast('消息已提交，turn=' + ack.turnId.slice(0, 8) + '…');
      }
    });
  }

  function newChat() {
    if (!ready) {
      UI.toast('连接未就绪，稍后再试');
      return;
    }
    // instanceId/cwd 留空 = 本机（payload 三字段全可选）。
    sendAction(H.createChat(), 'create', function (ack) {
      // chatId 已在 onAck 里统一处理（自动订阅选中）
      if (!ack.chatId) UI.toast('create committed 缺少 chatId');
    });
  }

  function cancelTurn() {
    if (!ready) {
      UI.toast('连接未就绪，稍后再试');
      return;
    }
    if (!currentCid) return;
    if (isTerminal(chatStatus[currentCid])) {
      UI.toast('对话已结束，无需取消');
      return;
    }
    sendAction(H.cancel(currentCid), 'cancel');
  }

  function closeChat() {
    if (!ready) {
      UI.toast('连接未就绪，稍后再试');
      return;
    }
    if (!currentCid) return;
    if (isTerminal(chatStatus[currentCid])) {
      UI.toast('对话已结束，无需关闭');
      return;
    }
    sendAction(H.close(currentCid), 'close');
  }

  function resolvePermission(permissionId, decision) {
    if (!ready) {
      UI.toast('连接未就绪，稍后再试');
      return;
    }
    if (!currentCid) return;
    sendAction(H.resolvePermission(currentCid, permissionId, decision), 'resolve');
  }

  // ── 装配 ────────────────────────────────────────────────────────────────

  var initialToken = resolveToken();
  UI.setToken(initialToken);

  UI.init({
    connect: connect,
    disconnect: disconnect,
    send: sendMessage,
    newChat: newChat,
    cancelTurn: cancelTurn,
    closeChat: closeChat,
    selectChat: selectChat,
    resolvePermission: resolvePermission,
  });
})();
