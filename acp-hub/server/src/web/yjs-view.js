// acp-hub Web 面板 —— yjs 渲染模块。
//
// 职责（M3 方案 §3/§6）：
//   - DocStore：docId → Y.Doc 的缓存（registry 一个、每个对话 chat 一个、
//     可选 control 一个）；applyUpdateFrame 用 **Y.applyUpdate（v1）** 应用
//     yrs 快照/增量（server 侧 encode_state_as_update_v1，勿用 applyUpdateV2），
//     应用后经 requestAnimationFrame 合帧触发 onUpdate(docId)。
//   - 纯渲染函数：renderRegistry / renderChat / renderControl —— 从 Y.Doc
//     提取普通 JS 数据（Y.Text 需 toString() 取全文；created_at 空串兜底），
//     DOM 交由 ui.js 处理。
//
// 快照与增量同帧同处理（区别仅 projectionVersion 有无，此处不关心）。

(function () {
  'use strict';

  // ── DocStore：doc 生命周期 + 渲染调度 ──────────────────────────────────

  function DocStore() {
    this.docs = new Map();      // docId → Y.Doc
    this.rafPending = new Set(); // 本帧已排队的 docId（合帧去重）
    this.onUpdate = null;       // (docId) => void，main.js 注册渲染入口
  }

  // 取（或创建）docId 对应的 Y.Doc。多标签页各自独立连接 + 独立 Y.Doc，
  // server 单写者 + CRDT 收敛 → 天然一致（M3 §3.1 原则 4）。
  DocStore.prototype.docFor = function (docId) {
    var doc = this.docs.get(docId);
    if (!doc) {
      doc = new Y.Doc();
      this.docs.set(docId, doc);
    }
    return doc;
  };

  // 应用一帧 ysync.update（快照/增量同路径）：v1 解码 + applyUpdate + 合帧。
  // 单帧解码/应用失败不应中断渲染链：warn 后强制重渲染该 doc（已有内容
  // 的 doc 继续显示旧渲染；损坏帧可能丢失，快照/增量随后续帧或重连补齐）。
  DocStore.prototype.applyUpdateFrame = function (frame) {
    var doc = this.docFor(frame.doc);
    try {
      var bytes = window.HubProtocol.base64ToBytes(frame.update);
      Y.applyUpdate(doc, bytes); // v1，与 server encode_state_as_update_v1 对齐
    } catch (err) {
      console.warn('applyUpdateFrame 失败（doc=' + frame.doc + '）', err);
    }
    this.scheduleRender(frame.doc);
  };

  // rAF 合帧：同一帧内对同一 doc 的多次更新只触发一次渲染。
  DocStore.prototype.scheduleRender = function (docId) {
    var self = this;
    if (this.rafPending.has(docId)) return;
    this.rafPending.add(docId);
    requestAnimationFrame(function () {
      self.rafPending.delete(docId);
      if (self.onUpdate) self.onUpdate(docId);
    });
  };

  // ── 工具 ────────────────────────────────────────────────────────────────

  // Y.Text 全文（可能为 Y.Text 对象；其他类型兜底为 null）。
  function yText(value) {
    if (value && typeof value.toString === 'function' && !(value instanceof Y.Map) && !(value instanceof Y.Array)) {
      return value.toString();
    }
    return null;
  }

  // RFC3339 字符串兜底：assistant 增量骨架可能为空串（M3 §8），统一返回
  // '—' 避免渲染空白。
  function safeTime(s) {
    return s ? s : '—';
  }

  // ── renderRegistry：实例列表（hub:registry 投影，M3 §2.1）──────────

  // 根 Map 字段：instances / chats / global / schema_version /
  // projection_version。chats 是「活跃对话列表权威源」。
  function renderRegistry(doc) {
    var root = doc.getMap('root');
    var instances = [];
    var chats = [];
    var globalStatus = 'unknown';

    var m = root.get('instances');
    if (m instanceof Y.Map) {
      m.forEach(function (v, id) {
        instances.push({
          id: id,
          hostname: v.get('hostname'),
          status: v.get('status'), // "online"|"offline"|"unknown"
          tokenId: v.get('token_id'),
          registeredAt: v.get('registered_at'),
          lastHeartbeat: v.get('last_heartbeat'),
          chatCount: v.get('chat_count'),
        });
      });
    }

    var s = root.get('chats');
    if (s instanceof Y.Map) {
      s.forEach(function (v, id) {
        chats.push({
          id: id,
          instanceId: v.get('instance_id'),
          title: v.get('title'),
          status: v.get('status'), // "accepting"|"active"|"ended"|"closed"|"crashed"
          gap: v.get('gap'),
          updatedAt: v.get('updated_at'),
        });
      });
    }

    var g = root.get('global');
    if (g instanceof Y.Map) {
      globalStatus = g.get('status') || 'unknown'; // healthy|degraded|restarting
    }

    return {
      instances: instances,
      chats: chats,
      globalStatus: globalStatus,
      schemaVersion: root.get('schema_version'),
      projectionVersion: root.get('projection_version'),
    };
  }

  // ── renderChat：对话消息列表（chat:{cid} 投影，M3 §3.2）──────────────────

  // 渲染主序为 entry_order（Y.Array<String>）；entries 为 Y.Map<entryId, Y.Map>；
  // text 块是 Y.Text 对象，必须 toString()（schema 镜像里的 String 只是 serde
  // 镜像类型，yrs 侧是 TextPrelim）。tool_call 块经根 tool_calls 查名称/状态。
  function renderChat(doc) {
    var root = doc.getMap('root');
    var order = root.get('entry_order');
    var entries = root.get('entries');
    var toolCalls = root.get('tool_calls');
    var out = [];

    if (order instanceof Y.Array) {
      order.toArray().forEach(function (entryId) {
        var e = entries instanceof Y.Map ? entries.get(entryId) : null;
        if (!(e instanceof Y.Map)) return;
        var item = {
          id: entryId,
          turnId: e.get('turn_id'),
          kind: e.get('kind'),       // "message"|"tool"|"system"
          role: e.get('role'),       // "user"|"assistant"|"system"
          status: e.get('status'),   // pending|streaming|completed|cancelled|error
          authorUserId: e.get('author_user_id'),
          createdAt: safeTime(e.get('created_at')), // 空串兜底
          completedAt: e.get('completed_at') || null,
          text: '',
          reasoning: [],
          toolCalls: [],
          resources: [],
          error: null,
        };

        var err = e.get('error');
        if (err instanceof Y.Map) {
          item.error = { code: err.get('code'), message: err.get('message') };
        }

        var blockOrder = e.get('block_order');
        var blocks = e.get('blocks');
        if (blockOrder instanceof Y.Array) {
          blockOrder.toArray().forEach(function (blockId) {
            var b = blocks instanceof Y.Map ? blocks.get(blockId) : null;
            if (!(b instanceof Y.Map)) return;
            var kind = b.get('kind');
            if (kind === 'text') {
              var t = yText(b.get('text'));
              if (t !== null) item.text += t;
            } else if (kind === 'reasoning') {
              item.reasoning.push({
                text: yText(b.get('text')) || '',
                visibility: b.get('visibility') || 'summary', // summary|hidden
              });
            } else if (kind === 'tool_call') {
              var tcId = b.get('tool_call_id');
              var tc = tcId && toolCalls instanceof Y.Map ? toolCalls.get(tcId) : null;
              item.toolCalls.push({
                toolCallId: tcId,
                name: tc instanceof Y.Map ? tc.get('name') : null,
                status: tc instanceof Y.Map ? tc.get('status') : null,
              });
            } else if (kind === 'resource') {
              item.resources.push({
                resourceId: b.get('resource_id'),
                mediaType: b.get('media_type'),
                name: b.get('name'),
              });
            }
          });
        }
        out.push(item);
      });
    }

    return {
      schemaVersion: root.get('schema_version'),
      projectionVersion: root.get('projection_version'),
      entries: out,
    };
  }

  // ── renderControl：对话头部 + 权限请求（control:{cid} 投影，M3 §3.3）─────

  // 根 Map：chat / agent / active_turn / pending_permissions。权限条
  // （allow/deny → permission/resolve）数据取自 pending_permissions。
  function renderControl(doc) {
    var root = doc.getMap('root');
    var result = {
      chat: null,
      agent: null,
      activeTurn: null,
      pendingPermissions: [],
    };

    var sess = root.get('chat');
    if (sess instanceof Y.Map) {
      result.chat = {
        // chat_id 键 server 侧暂不写入（ChatInfoProjection 仅是 serde
        // 镜像，实际写入见 aggregator.rs write_chat_info），读空兜底 '',
        // 避免 UI 层对 undefined 取 slice 抛 TypeError。
        chatId: sess.get('chat_id') || '',
        title: sess.get('title'),
        status: sess.get('status'),
        activeTurnId: sess.get('active_turn_id'),
        createdAt: sess.get('created_at'),
        updatedAt: sess.get('updated_at'),
      };
    }

    var agent = root.get('agent');
    if (agent instanceof Y.Map) {
      var caps = agent.get('capabilities');
      result.agent = {
        instanceId: agent.get('instance_id'),
        sessionId: agent.get('session_id'),
        status: agent.get('status'),
        lastActivityAt: agent.get('last_activity_at'),
        capabilities: caps instanceof Y.Array ? caps.toArray() : [],
      };
    }

    var turn = root.get('active_turn');
    if (turn instanceof Y.Map) {
      result.activeTurn = {
        turnId: turn.get('turn_id'),
        turnStatus: turn.get('turn_status'),
        updatedAt: turn.get('updated_at'),
      };
    }

    var perms = root.get('pending_permissions');
    if (perms instanceof Y.Map) {
      perms.forEach(function (p) {
        if (!(p instanceof Y.Map)) return;
        result.pendingPermissions.push({
          permissionId: p.get('permission_id'),
          turnId: p.get('turn_id'),
          toolCallId: p.get('tool_call_id'),
          title: p.get('title'),
          description: p.get('description'),
          status: p.get('status'),
          expiresAt: p.get('expires_at'),
          decision: p.get('decision'),
        });
      });
    }

    return result;
  }

  // ── 导出 ────────────────────────────────────────────────────────────────

  window.YjsView = {
    DocStore: DocStore,
    renderRegistry: renderRegistry,
    renderChat: renderChat,
    renderControl: renderControl,
  };
})();
