// acp-hub Web 面板 —— yjs 渲染模块（移植自原 yjs-view.js，TS 化）。
//
// 职责（M3 方案 §3/§6）：
//   - DocStore：docId → Y.Doc 的缓存（registry 一个、每个对话 chat 一个、
//     可选 control 一个）；applyUpdateFrame 用 **Y.applyUpdate（v1）** 应用
//     yrs 快照/增量（server 侧 encode_state_as_update_v1，勿用 applyUpdateV2），
//     应用后经 requestAnimationFrame 合帧触发 onUpdate(docId)。
//   - 纯渲染函数：renderRegistry / renderChat / renderControl —— 从 Y.Doc
//     提取普通 JS 数据（Y.Text 需 toString() 取全文；created_at 空串兜底），
//     DOM 交由组件处理。
//
// 快照与增量同帧同处理（区别仅 projectionVersion 有无，此处不关心）。

import * as Y from 'yjs';
import { base64ToBytes } from './protocol';

// ── DocStore：doc 生命周期 + 渲染调度 ──────────────────────────────────

export class DocStore {
  private docs = new Map<string, Y.Doc>();
  private rafPending = new Set<string>(); // 本帧已排队的 docId（合帧去重）
  /** (docId) => void，main 注册渲染入口。 */
  onUpdate: ((docId: string) => void) | null = null;

  /** 取（或创建）docId 对应的 Y.Doc。多标签页各自独立连接 + 独立 Y.Doc，
   *  server 单写者 + CRDT 收敛 → 天然一致（M3 §3.1 原则 4）。 */
  docFor(docId: string): Y.Doc {
    let doc = this.docs.get(docId);
    if (!doc) {
      doc = new Y.Doc();
      this.docs.set(docId, doc);
    }
    return doc;
  }

  /** 应用一帧 ysync.update（快照/增量同路径）：v1 解码 + applyUpdate + 合帧。
   *  单帧解码/应用失败不应中断渲染链：warn 后强制重渲染该 doc（已有内容
   *  的 doc 继续显示旧渲染；损坏帧可能丢失，快照/增量随后续帧或重连补齐）。 */
  applyUpdateFrame(frame: { doc: string; update: string }): void {
    const doc = this.docFor(frame.doc);
    try {
      const bytes = base64ToBytes(frame.update);
      Y.applyUpdate(doc, bytes); // v1，与 server encode_state_as_update_v1 对齐
    } catch (err) {
      console.warn(`applyUpdateFrame 失败（doc=${frame.doc}）`, err);
    }
    this.scheduleRender(frame.doc);
  }

  /** rAF 合帧：同一帧内对同一 doc 的多次更新只触发一次渲染。 */
  private scheduleRender(docId: string): void {
    if (this.rafPending.has(docId)) return;
    this.rafPending.add(docId);
    requestAnimationFrame(() => {
      this.rafPending.delete(docId);
      if (this.onUpdate) this.onUpdate(docId);
    });
  }
}

// ── 工具 ────────────────────────────────────────────────────────────────

/** Y.Text 全文（可能为 Y.Text 对象；其他类型兜底为 null）。 */
function yText(value: unknown): string | null {
  if (
    value &&
    typeof (value as { toString?: unknown }).toString === 'function' &&
    !(value instanceof Y.Map) &&
    !(value instanceof Y.Array)
  ) {
    return (value as { toString(): string }).toString();
  }
  return null;
}

/** RFC3339 字符串兜底：assistant 增量骨架可能为空串（M3 §8），统一返回
 *  '—' 避免渲染空白。 */
function safeTime(s: unknown): string {
  return s ? String(s) : '—';
}

function asMap(v: unknown): Y.Map<unknown> | null {
  return v instanceof Y.Map ? v : null;
}

function asArray(v: unknown): Y.Array<unknown> | null {
  return v instanceof Y.Array ? v : null;
}

function getStr(m: Y.Map<unknown> | null, key: string): string | null {
  const v = m ? m.get(key) : null;
  return v === undefined || v === null ? null : String(v);
}

// ── 渲染结果类型 ────────────────────────────────────────────────────────

export interface InstanceInfo {
  id: string;
  hostname: string | null;
  status: string | null;
  tokenId: string | null;
  registeredAt: string | null;
  lastHeartbeat: string | null;
  chatCount: unknown;
}

export interface ChatInfo {
  id: string;
  instanceId: string | null;
  title: string | null;
  status: string | null;
  gap: unknown;
  updatedAt: string | null;
}

export interface RegistryView {
  instances: InstanceInfo[];
  chats: ChatInfo[];
  globalStatus: string;
  schemaVersion: unknown;
  projectionVersion: unknown;
}

export interface ReasoningBlock {
  text: string;
  visibility: string | null;
}

export interface ToolCallInfo {
  toolCallId: string | null;
  name: string | null;
  status: string | null;
}

export interface ResourceInfo {
  resourceId: string | null;
  mediaType: string | null;
  name: string | null;
}

export interface ChatEntry {
  id: string;
  turnId: string | null;
  kind: string | null;
  role: string | null;
  status: string | null;
  authorUserId: string | null;
  createdAt: string;
  completedAt: string | null;
  text: string;
  reasoning: ReasoningBlock[];
  toolCalls: ToolCallInfo[];
  resources: ResourceInfo[];
  error: { code: string | null; message: string | null } | null;
}

export interface ChatView {
  schemaVersion: unknown;
  projectionVersion: unknown;
  entries: ChatEntry[];
}

export interface ChatHeadInfo {
  chatId: string;
  title: string | null;
  status: string | null;
  activeTurnId: string | null;
  createdAt: string | null;
  updatedAt: string | null;
}

export interface AgentInfo {
  instanceId: string | null;
  sessionId: string | null;
  status: string | null;
  lastActivityAt: string | null;
  capabilities: unknown[];
}

export interface ActiveTurnInfo {
  turnId: string | null;
  turnStatus: string | null;
  updatedAt: string | null;
}

export interface PendingPermission {
  permissionId: string | null;
  turnId: string | null;
  toolCallId: string | null;
  title: string | null;
  description: string | null;
  status: string | null;
  expiresAt: string | null;
  decision: string | null;
}

export interface ControlView {
  chat: ChatHeadInfo | null;
  agent: AgentInfo | null;
  activeTurn: ActiveTurnInfo | null;
  pendingPermissions: PendingPermission[];
}

// ── renderRegistry：实例列表（hub:registry 投影，M3 §2.1）──────────────

/** 根 Map 字段：instances / chats / global / schema_version /
 *  projection_version。chats 是「活跃对话列表权威源」。 */
export function renderRegistry(doc: Y.Doc): RegistryView {
  const root = doc.getMap<unknown>('root');
  const instances: InstanceInfo[] = [];
  const chats: ChatInfo[] = [];
  let globalStatus = 'unknown';

  const m = asMap(root.get('instances'));
  if (m) {
    m.forEach((v, id) => {
      const im = asMap(v);
      instances.push({
        id,
        hostname: getStr(im, 'hostname'),
        status: getStr(im, 'status'), // "online"|"offline"|"unknown"
        tokenId: getStr(im, 'token_id'),
        registeredAt: getStr(im, 'registered_at'),
        lastHeartbeat: getStr(im, 'last_heartbeat'),
        chatCount: im ? im.get('chat_count') : null,
      });
    });
  }

  const s = asMap(root.get('chats'));
  if (s) {
    s.forEach((v, id) => {
      const sm = asMap(v);
      chats.push({
        id,
        instanceId: getStr(sm, 'instance_id'),
        title: getStr(sm, 'title'),
        status: getStr(sm, 'status'), // "accepting"|"active"|"ended"|"closed"|"crashed"
        gap: sm ? sm.get('gap') : null,
        updatedAt: getStr(sm, 'updated_at'),
      });
    });
  }

  const g = asMap(root.get('global'));
  if (g) {
    globalStatus = getStr(g, 'status') || 'unknown'; // healthy|degraded|restarting
  }

  return {
    instances,
    chats,
    globalStatus,
    schemaVersion: root.get('schema_version'),
    projectionVersion: root.get('projection_version'),
  };
}

// ── renderChat：对话消息列表（chat:{cid} 投影，M3 §3.2）─────────────────

/** 渲染主序为 entry_order（Y.Array<String>）；entries 为 Y.Map<entryId,
 *  Y.Map>。text 块是 Y.Text 对象，必须 toString()（schema 镜像里的 String
 *  只是 serde 镜像类型，yrs 侧是 TextPrelim）。tool_call 块经根 tool_calls
 *  查名称/状态。 */
export function renderChat(doc: Y.Doc): ChatView {
  const root = doc.getMap<unknown>('root');
  const order = asArray(root.get('entry_order'));
  const entriesMap = asMap(root.get('entries'));
  const toolCalls = asMap(root.get('tool_calls'));
  const out: ChatEntry[] = [];

  if (order) {
    order.toArray().forEach((entryId) => {
      const e = entriesMap ? entriesMap.get(entryId as string) : null;
      const em = asMap(e);
      if (!em) return;
      const item: ChatEntry = {
        id: entryId as string,
        turnId: getStr(em, 'turn_id'),
        kind: getStr(em, 'kind'), // "message"|"tool"|"system"
        role: getStr(em, 'role'), // "user"|"assistant"|"system"
        status: getStr(em, 'status'), // pending|streaming|completed|cancelled|error
        authorUserId: getStr(em, 'author_user_id'),
        createdAt: safeTime(em.get('created_at')), // 空串兜底
        completedAt: getStr(em, 'completed_at'),
        text: '',
        reasoning: [],
        toolCalls: [],
        resources: [],
        error: null,
      };

      const err = asMap(em.get('error'));
      if (err) {
        item.error = { code: getStr(err, 'code'), message: getStr(err, 'message') };
      }

      const blockOrder = asArray(em.get('block_order'));
      const blocks = asMap(em.get('blocks'));
      if (blockOrder) {
        blockOrder.toArray().forEach((blockId) => {
          const b = blocks ? blocks.get(blockId as string) : null;
          const bm = asMap(b);
          if (!bm) return;
          const kind = bm.get('kind');
          if (kind === 'text') {
            const t = yText(bm.get('text'));
            if (t !== null) item.text += t;
          } else if (kind === 'reasoning') {
            item.reasoning.push({
              text: yText(bm.get('text')) || '',
              visibility: getStr(bm, 'visibility'), // summary|hidden
            });
          } else if (kind === 'tool_call') {
            const tcId = bm.get('tool_call_id') as string | null;
            const tc = tcId && toolCalls ? toolCalls.get(tcId) : null;
            const tcm = asMap(tc);
            item.toolCalls.push({
              toolCallId: tcId,
              name: getStr(tcm, 'name'),
              status: getStr(tcm, 'status'),
            });
          } else if (kind === 'resource') {
            item.resources.push({
              resourceId: bm.get('resource_id') as string | null,
              mediaType: bm.get('media_type') as string | null,
              name: bm.get('name') as string | null,
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

/** 根 Map：chat / agent / active_turn / pending_permissions。权限条
 *  （allow/deny → permission/resolve）数据取自 pending_permissions。 */
export function renderControl(doc: Y.Doc): ControlView {
  const root = doc.getMap<unknown>('root');
  const result: ControlView = {
    chat: null,
    agent: null,
    activeTurn: null,
    pendingPermissions: [],
  };

  const sess = asMap(root.get('chat'));
  if (sess) {
    // chat_id 键 server 侧暂不写入（ChatInfoProjection 仅是 serde 镜像，
    // 实际写入见 aggregator.rs write_chat_info），读空兜底 ''，避免 UI 层
    // 对 undefined 取 slice 抛 TypeError。
    result.chat = {
      chatId: getStr(sess, 'chat_id') || '',
      title: getStr(sess, 'title'),
      status: getStr(sess, 'status'),
      activeTurnId: getStr(sess, 'active_turn_id'),
      createdAt: getStr(sess, 'created_at'),
      updatedAt: getStr(sess, 'updated_at'),
    };
  }

  const agent = asMap(root.get('agent'));
  if (agent) {
    const caps = asArray(agent.get('capabilities'));
    result.agent = {
      instanceId: getStr(agent, 'instance_id'),
      sessionId: getStr(agent, 'session_id'),
      status: getStr(agent, 'status'),
      lastActivityAt: getStr(agent, 'last_activity_at'),
      capabilities: caps ? caps.toArray() : [],
    };
  }

  const turn = asMap(root.get('active_turn'));
  if (turn) {
    result.activeTurn = {
      turnId: getStr(turn, 'turn_id'),
      turnStatus: getStr(turn, 'turn_status'),
      updatedAt: getStr(turn, 'updated_at'),
    };
  }

  const perms = asMap(root.get('pending_permissions'));
  if (perms) {
    perms.forEach((p) => {
      const pm = asMap(p);
      if (!pm) return;
      result.pendingPermissions.push({
        permissionId: getStr(pm, 'permission_id'),
        turnId: getStr(pm, 'turn_id'),
        toolCallId: getStr(pm, 'tool_call_id'),
        title: getStr(pm, 'title'),
        description: getStr(pm, 'description'),
        status: getStr(pm, 'status'),
        expiresAt: getStr(pm, 'expires_at'),
        decision: getStr(pm, 'decision'),
      });
    });
  }

  return result;
}
