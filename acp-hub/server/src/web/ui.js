// acp-hub Web 面板 —— DOM 模块。
//
// 职责（M3 方案 §4）：渲染左栏（token 表单/实例/对话）、右区（对话头部 +
// 工具栏 + 权限条 + 消息气泡 + 输入框）与状态 rail（连接状态机/心跳/最近
// ack/最近错误），以及 toast。数据由 yjs-view.js 的纯渲染函数产出，本模块
// 只做 DOM，不含任何协议逻辑。
//
// 回调接口（init 时注册）：
//   connect(token)                     —— 点「连接」按钮
//   disconnect()                       —— 点「断开」按钮
//   send(text)                         —— 发送消息（按钮/Enter）
//   newChat()                       —— 新建对话
//   cancelTurn() / closeChat()      —— 取消当前 turn / 关闭对话
//   selectChat(cid)                 —— 点击对话列表项
//   resolvePermission(permissionId, decision)

(function () {
  'use strict';

  function PanelUI() {
    this.cbs = {};
    this.stickToBottom = true; // 自动吸底；用户上滚时暂停
    this._cache();
    this._bind();
  }

  // ── 元素缓存 ────────────────────────────────────────────────────────────

  PanelUI.prototype._cache = function () {
    this.$ = {};
    var ids = [
      'conn-state', 'heartbeat-count', 'global-status', 'subscribed-docs',
      'token-input', 'connect-btn', 'disconnect-btn',
      'instance-list', 'instance-count', 'chat-list', 'chat-count',
      'chat-title', 'chat-meta', 'new-chat-btn', 'cancel-btn', 'close-btn',
      'permission-bar', 'perm-title', 'perm-desc', 'perm-allow', 'perm-deny',
      'chat-area', 'message-input', 'send-btn',
      'ack-log', 'error-log', 'toast-root', 'yjs-offline',
    ];
    var self = this;
    ids.forEach(function (id) {
      self.$[id] = document.getElementById(id);
    });
  };

  // ── 事件绑定 ────────────────────────────────────────────────────────────

  // 注册回调表（main.js 装配时调用；connect/send/selectChat 等）。
  PanelUI.prototype.init = function (callbacks) {
    this.cbs = callbacks || {};
  };

  PanelUI.prototype._bind = function () {
    var self = this;
    var $ = this.$;

    $['connect-btn'].addEventListener('click', function () {
      self._emit('connect', $['token-input'].value.trim());
    });
    $['disconnect-btn'].addEventListener('click', function () {
      self._emit('disconnect');
    });

    $['send-btn'].addEventListener('click', function () {
      self._submitMessage();
    });
    $['message-input'].addEventListener('keydown', function (ev) {
      if (ev.key === 'Enter' && !ev.shiftKey) {
        ev.preventDefault();
        self._submitMessage();
      }
    });

    $['new-chat-btn'].addEventListener('click', function () {
      self._emit('newChat');
    });
    $['cancel-btn'].addEventListener('click', function () {
      self._emit('cancelTurn');
    });
    $['close-btn'].addEventListener('click', function () {
      self._emit('closeChat');
    });

    // 对话列表：事件委托（列表项 data-cid）。
    $['chat-list'].addEventListener('click', function (ev) {
      var li = ev.target.closest('.panel-chat');
      if (li) self._emit('selectChat', li.getAttribute('data-cid'));
    });

    // 权限条：allow / deny。
    $['perm-allow'].addEventListener('click', function () {
      self._resolvePermission('allow');
    });
    $['perm-deny'].addEventListener('click', function () {
      self._resolvePermission('deny');
    });

    // 消息区吸底：距底部 < 40px 视为吸底模式，用户上滚则暂停。
    $['chat-area'].addEventListener('scroll', function () {
      var el = $['chat-area'];
      self.stickToBottom =
        el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    });
  };

  PanelUI.prototype._emit = function (name) {
    var cb = this.cbs[name];
    var args = Array.prototype.slice.call(arguments, 1);
    if (typeof cb === 'function') cb.apply(null, args);
  };

  PanelUI.prototype._resolvePermission = function (decision) {
    var permId = this.$['permission-bar'].getAttribute('data-permission-id');
    if (permId) this._emit('resolvePermission', permId, decision);
  };

  PanelUI.prototype._submitMessage = function () {
    var input = this.$['message-input'];
    var text = input.value.trim();
    if (!text) return;
    input.value = '';
    this._emit('send', text);
  };

  // ── 连接表单 ────────────────────────────────────────────────────────────

  PanelUI.prototype.getToken = function () {
    return this.$['token-input'].value.trim();
  };

  PanelUI.prototype.setToken = function (token) {
    this.$['token-input'].value = token || '';
  };

  // busy=true：连接/断开按钮互斥（连接中禁连、断开中禁断）。
  PanelUI.prototype.setBusy = function (busy) {
    this.$['connect-btn'].disabled = busy;
    this.$['disconnect-btn'].disabled = busy;
  };

  // ── 状态区（rail）───────────────────────────────────────────────────────

  // kind: '' | 'ok' | 'warn' | 'err' —— 徽标配色。
  PanelUI.prototype.setConnState = function (text, kind) {
    this.$['conn-state'].textContent = text;
    this.$['conn-state'].className = 'panel-badge ' + (kind ? 'badge-' + kind : '');
  };

  PanelUI.prototype.setReady = function (projectionVersions) {
    var names = projectionVersions
      ? Object.keys(projectionVersions).join('、')
      : '—';
    this.$['subscribed-docs'].textContent = names;
  };

  PanelUI.prototype.bumpHeartbeat = function () {
    var el = this.$['heartbeat-count'];
    el.textContent = parseInt(el.textContent || '0', 10) + 1;
  };

  PanelUI.prototype.setGlobalStatus = function (status) {
    var el = this.$['global-status'];
    el.textContent = status || '—';
    var kind = status === 'healthy' ? 'ok' : status === 'degraded' ? 'warn' : 'err';
    el.className = 'panel-badge ' + (status ? 'badge-' + kind : '');
  };

  // 最近 ack / 最近错误：追加一条，最多保留 5 条。
  PanelUI.prototype.showAck = function (ack) {
    var cid = ack.commandId || '';
    var short = cid.length > 8 ? cid.slice(0, 8) + '…' : cid;
    var line = short + ' → ' + ack.status;
    if (ack.chatId) line += ' · cid=' + ack.chatId.slice(0, 8) + '…';
    if (ack.turnId) line += ' · turn=' + ack.turnId.slice(0, 8) + '…';
    this._pushLog(this.$['ack-log'], line, ack.status);
  };

  PanelUI.prototype.showActionError = function (err) {
    var cid = err.commandId || '';
    var short = cid.length > 8 ? cid.slice(0, 8) + '…' : cid;
    this._pushLog(
      this.$['error-log'],
      short + ' · ' + err.code + (err.message ? ' · ' + err.message : ''),
      'err'
    );
  };

  PanelUI.prototype._pushLog = function (ul, text, cls) {
    var li = document.createElement('li');
    li.className = 'panel-log-line' + (cls ? ' log-' + cls : '');
    li.textContent = text;
    ul.appendChild(li);
    while (ul.children.length > 5) {
      ul.removeChild(ul.firstChild);
    }
  };

  // ── 左栏列表 ────────────────────────────────────────────────────────────

  // 实例列表：hostname + 状态徽标 + 对话数。
  PanelUI.prototype.renderInstances = function (instances) {
    var $ = this.$;
    $['instance-count'].textContent = '(' + instances.length + ')';
    $['instance-list'].textContent = '';
    instances
      .slice()
      .sort(function (a, b) {
        return String(a.hostname).localeCompare(String(b.hostname));
      })
      .forEach(function (m) {
        var li = document.createElement('li');
        li.className = 'panel-li';
        var title = document.createElement('div');
        title.className = 'panel-li-title';
        title.textContent = m.hostname || m.id || '?';
        title.appendChild(this._badge(m.status || 'unknown'));
        li.appendChild(title);
        var meta = document.createElement('div');
        meta.className = 'panel-li-meta';
        meta.textContent =
          '对话 ' + (m.chatCount === null || m.chatCount === undefined ? '—' : m.chatCount) +
          ' · ' + ((m.id || '').slice(0, 8) || '—');
        li.appendChild(meta);
        $['instance-list'].appendChild(li);
      }, this);
  };

  // 对话列表：title + 状态徽标 + 更新时间；点击经事件委托上抛 selectChat。
  PanelUI.prototype.renderChats = function (chats, selectedCid) {
    var $ = this.$;
    $['chat-count'].textContent = '(' + chats.length + ')';
    $['chat-list'].textContent = '';
    chats
      .slice()
      .sort(function (a, b) {
        return String(b.updatedAt || '').localeCompare(String(a.updatedAt || ''));
      })
      .forEach(function (s) {
        var li = document.createElement('li');
        li.className = 'panel-li panel-chat';
        if (s.id === selectedCid) li.className += ' selected';
        li.setAttribute('data-cid', s.id);
        var title = document.createElement('div');
        title.className = 'panel-li-title';
        title.textContent = s.title || s.id.slice(0, 8) + '…';
        title.appendChild(this._badge(s.status || 'unknown'));
        li.appendChild(title);
        var meta = document.createElement('div');
        meta.className = 'panel-li-meta';
        meta.textContent = this._shortTime(s.updatedAt);
        li.appendChild(meta);
        $['chat-list'].appendChild(li);
      }, this);
  };

  // ── 右侧：对话头部 + 工具栏 ─────────────────────────────────────────────

  PanelUI.prototype.setCurrentChat = function (cid, status) {
    var title = this.$['chat-title'];
    var meta = this.$['chat-meta'];
    // 终态对话（ended/closed/crashed，§5.5）：ACP 进程已退出，只读历史。
    var terminal = cid &&
      (status === 'ended' || status === 'closed' || status === 'crashed');
    if (cid) {
      title.textContent = '对话 ' + cid.slice(0, 8) + '…' + (terminal ? '（已结束）' : '');
      meta.textContent = cid + (terminal ? ' · ' + status : '');
      this.$['cancel-btn'].disabled = terminal;
      this.$['close-btn'].disabled = terminal;
      this.$['message-input'].disabled = terminal;
      this.$['send-btn'].disabled = terminal;
      if (terminal) this.$['message-input'].placeholder = '对话已结束（历史只读）';
    } else {
      title.textContent = '未选择对话';
      meta.textContent = '点击左侧对话列表，或新建对话';
      this.$['cancel-btn'].disabled = true;
      this.$['close-btn'].disabled = true;
      this.$['message-input'].disabled = true;
      this.$['send-btn'].disabled = true;
    }
    // 对话切换：清空旧渲染，等快照到达后重新填充。
    this.$['chat-area'].textContent = '';
    this.$['chat-area'].setAttribute('data-cid', cid || '');
  };

  // control:{cid} 投影到达 → 头部标题/状态。
  PanelUI.prototype.renderChatHead = function (data) {
    if (!data || !data.chat) return;
    var title = this.$['chat-title'];
    // 键名与 yjs-view.js renderControl 产出一致（chatId）；server 暂不
    // 写 chat_id 键 → yjs-view 兜底 ''，这里再兜底到「对话」占位。
    title.textContent = data.chat.title ||
      (data.chat.chatId ? data.chat.chatId.slice(0, 8) + '…' : '对话');
    var meta = this.$['chat-meta'];
    meta.textContent =
      data.chat.chatId +
      ' · ' + (data.chat.status || '—') +
      (data.activeTurn && data.activeTurn.turnStatus
        ? ' · turn=' + data.activeTurn.turnStatus
        : '');
    title.appendChild(this._badge(data.chat.status || 'unknown'));
  };

  // 权限条：pending_permissions 渲染 allow/deny；空列表隐藏。
  PanelUI.prototype.renderPermissions = function (perms) {
    var bar = this.$['permission-bar'];
    if (!perms || !perms.length) {
      bar.hidden = true;
      bar.removeAttribute('data-permission-id');
      return;
    }
    var p = perms[0]; // M3 雏形：逐条展示第一条，处理完移除
    bar.hidden = false;
    bar.setAttribute('data-permission-id', p.permissionId || '');
    this.$['perm-title'].textContent = p.title || '权限请求';
    this.$['perm-desc'].textContent =
      (p.description || '') + (p.toolCallId ? ' · tool=' + p.toolCallId.slice(0, 8) + '…' : '');
  };

  // ── 右侧：消息气泡 ──────────────────────────────────────────────────────

  // conv = renderChat(doc) 的返回值：{schemaVersion, projectionVersion, entries}。
  // 渲染规则（M3 §3.2）：role=user 右气泡、assistant 左气泡（text 全文 +
  // reasoning 折叠 + tool_call 摘要行）；streaming 加光标；error 显示 code。
  PanelUI.prototype.renderChat = function (conv) {
    var area = this.$['chat-area'];
    if (!conv) return;
    area.textContent = '';

    var self = this;
    conv.entries.forEach(function (e) {
      var wrap = document.createElement('div');
      wrap.className = 'panel-msg msg-' + (e.role === 'user' ? 'user' : e.role === 'system' ? 'system' : 'assistant');

      // 头部：角色 · 时间 · 状态
      var head = document.createElement('div');
      head.className = 'panel-msg-head';
      head.textContent = (e.role || '?') + ' · ' + e.createdAt + ' · ';
      head.appendChild(self._badge(e.status || 'unknown'));
      wrap.appendChild(head);

      // 正文：text 全文
      if (e.text) {
        var body = document.createElement('div');
        body.className = 'panel-msg-body';
        body.textContent = e.text;
        if (e.status === 'streaming') body.className += ' streaming';
        wrap.appendChild(body);
      }

      // reasoning：折叠展示（hidden → 只显示标记，不泄内容）
      e.reasoning.forEach(function (r) {
        var det = document.createElement('details');
        det.className = 'panel-reasoning';
        var sum = document.createElement('summary');
        sum.textContent = r.visibility === 'hidden' ? '思考过程（hidden）' : '思考过程';
        det.appendChild(sum);
        var pre = document.createElement('pre');
        pre.textContent = r.text;
        det.appendChild(pre);
        wrap.appendChild(det);
      });

      // tool_call 摘要行
      e.toolCalls.forEach(function (tc) {
        var line = document.createElement('div');
        line.className = 'panel-toolcall';
        line.textContent = '工具调用: ' + (tc.name || tc.toolCallId || '?') + ' [' + (tc.status || '—') + ']';
        wrap.appendChild(line);
      });

      // resource 行
      e.resources.forEach(function (r) {
        var line = document.createElement('div');
        line.className = 'panel-resource';
        line.textContent = '资源: ' + (r.name || r.resourceId || '?') + ' (' + (r.mediaType || '—') + ')';
        wrap.appendChild(line);
      });

      // 错误显示
      if (e.error) {
        var err = document.createElement('div');
        err.className = 'panel-error';
        err.textContent = e.error.code + (e.error.message ? ': ' + e.error.message : '');
        wrap.appendChild(err);
      }

      // 空体（pending/streaming 骨架）→ 占位光标
      if (!e.text && !e.reasoning.length && !e.toolCalls.length && !e.error) {
        var ghost = document.createElement('div');
        ghost.className = 'panel-msg-body streaming';
        ghost.textContent = '…';
        wrap.appendChild(ghost);
      }

      area.appendChild(wrap);
    });

    // 自动吸底（用户上滚时暂停）
    if (this.stickToBottom) {
      area.scrollTop = area.scrollHeight;
    }
  };

  // ── 通用 ────────────────────────────────────────────────────────────────

  // toast：底部浮现，2.5s 自动消失。
  PanelUI.prototype.toast = function (msg) {
    var el = document.createElement('div');
    el.className = 'panel-toast';
    el.textContent = msg;
    this.$['toast-root'].appendChild(el);
    setTimeout(function () {
      el.remove();
    }, 2500);
  };

  // 状态徽标（badge-ok 绿 / badge-warn 黄 / badge-err 红 / 默认灰蓝）。
  PanelUI.prototype._badge = function (status) {
    var span = document.createElement('span');
    span.className = 'panel-badge badge-' + this._badgeKind(status);
    span.textContent = status;
    return span;
  };

  PanelUI.prototype._badgeKind = function (status) {
    var ok = ['online', 'healthy', 'completed', 'accepting', 'allow'];
    var warn = ['degraded', 'active', 'streaming', 'pending', 'awaitingPermission', 'running', 'deny'];
    var err = ['offline', 'crashed', 'error', 'restarting', 'failed'];
    if (ok.indexOf(status) >= 0) return 'ok';
    if (warn.indexOf(status) >= 0) return 'warn';
    if (err.indexOf(status) >= 0) return 'err';
    return 'neutral';
  };

  // RFC3339 → 本地 HH:MM:SS（空串/非法 → '—'）。
  PanelUI.prototype._shortTime = function (s) {
    if (!s || s === '—') return '—';
    var d = new Date(s);
    if (isNaN(d.getTime())) return '—';
    return d.toLocaleTimeString();
  };

  window.PanelUI = PanelUI;
})();
