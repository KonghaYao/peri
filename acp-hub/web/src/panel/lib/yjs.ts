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
  clear(): void { this.docs.forEach((doc) => doc.destroy()); this.docs.clear(); this.rafPending.clear(); }

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
    } catch {
      console.warn(`applyUpdateFrame 失败（doc=${frame.doc}, update_length=${frame.update.length}）`);
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

/** 数值兜底：yjs 数值字段直接取（serde 镜像为 number），缺失/非法 → null
 *  （渲染侧以 — 兜底）。 */
function getNum(m: Y.Map<unknown> | null, key: string): number | null {
  const v = m ? m.get(key) : null;
  if (v === undefined || v === null) return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

/** Convert nested Yjs values into inert plain data for presentation. The server
 * already redacts tool projections; this layer must not parse or execute them. */
function yValue(value: unknown): unknown {
  if (value instanceof Y.Map) {
    return Object.fromEntries([...value.entries()].map(([key, item]) => [key, yValue(item)]));
  }
  if (value instanceof Y.Array) return value.toArray().map(yValue);
  if (value instanceof Y.Text) return value.toString();
  return value;
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
  /** ACP 进程工作目录（继承自 workspace 或 server 默认目录，§6.3）。 */
  cwd: string | null;
  /** 归属工作区（无 → null；工作区删除后已建对话保留此引用）。 */
  workspaceId: string | null;
}

/** 工作区（独立于 chat 的上层概念，§6.3）：定义本地目录 cwd，其下新建
 *  对话继承该目录。Registry Doc `workspaces` map。 */
export interface WorkspaceInfo {
  id: string;
  name: string;
  cwd: string;
  createdAt: string | null;
  updatedAt: string | null;
}

export interface ProjectInfo {
  id: string;
  name: string;
  cwd: string;
  instanceId: string;
  createdAt: string | null;
  updatedAt: string | null;
  archivedAt: string | null;
}

export interface ProjectSessionInfo {
  id: string;
  projectId: string;
  acpSessionId: string | null;
  title: string;
  lifecycle: string;
  updatedAt: string | null;
  lastOpenedAt: string | null;
  activeChatId: string | null;
}

export interface RegistryView {
  instances: InstanceInfo[];
  chats: ChatInfo[];
  /** ACP agent 磁盘历史会话（instance 级数据，§6.3：server 经 session/list
   *  轮询投影到 Registry Doc `sessions` Map——全局共享，不随 chat 切换）。 */
  sessions: SessionSummaryInfo[];
  /** 工作区定义列表（§6.3 workspace 扩展）。 */
  workspaces: WorkspaceInfo[];
  projects: ProjectInfo[];
  projectSessions: ProjectSessionInfo[];
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
  arguments: unknown;
  result: unknown;
  /** null means a legacy projection that did not record omission provenance. */
  resultOmitted: boolean | null;
  resultBytes: number | null;
  publicError: { code: string | null; message: string | null } | null;
  /** Hub-observed event timestamps; absent on legacy projections. */
  startedAt: string | null;
  completedAt: string | null;
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
  /** 模型名（server 从 agent 元信息写入；无 → null，UI 显示 —）。 */
  model: string | null;
  /** 推理强度（server 从 agent 元信息写入；无 → null，UI 显示 —）。 */
  effort: string | null;
  /** 上下文窗口大小（tokens）。 */
  contextWindow: number | null;
  /** 已占用上下文（tokens）。 */
  contextUsed: number | null;
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

/** ACP 会话摘要（agent 磁盘历史，§5.4）：与 hub 对话（控制面）语义不同。
 *  server 经 session/list 轮询写入 **Registry Doc** `sessions` Map（instance
 *  级数据，§6.3：全局共享，不随 chat 切换销毁/重建）。 */
export interface SessionSummaryInfo {
  sessionId: string;
  title: string | null;
  status: string | null;
  updatedAt: string | null;
  /** 会话所在 ACP 进程工作目录（§6.3 workspace 扩展：按 cwd 分面过滤）。 */
  cwd: string | null;
  /** §8.5 会话切换语义：会话是**进程内实体**——该会话是否为当前对话
   *  的**当前活跃会话**（= 本对话 chat_id；前端标「当前」徽标，点击无
   *  操作）。其余会话该字段为 null（历史会话，点击 = 当前对话内 load）。 */
  boundChatId?: string | null;
}

export interface ControlView {
  chat: ChatHeadInfo | null;
  agent: AgentInfo | null;
  activeTurn: ActiveTurnInfo | null;
  pendingPermissions: PendingPermission[];
}

// ── renderRegistry：实例列表（hub:registry 投影，M3 §2.1）──────────────

/** 根 Map 字段：instances / chats / global / sessions / schema_version /
 *  projection_version。chats 是「活跃对话列表权威源」；sessions 是 instance
 *  级 ACP 历史会话（§6.3 轮询投影）。 */
export function renderRegistry(doc: Y.Doc): RegistryView {
  const root = doc.getMap<unknown>('root');
  const instances: InstanceInfo[] = [];
  const chats: ChatInfo[] = [];
  const sessions: SessionSummaryInfo[] = [];
  const workspaces: WorkspaceInfo[] = [];
  const projects: ProjectInfo[] = [];
  const projectSessions: ProjectSessionInfo[] = [];
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
        cwd: getStr(sm, 'cwd'),
        workspaceId: getStr(sm, 'workspace_id'),
      });
    });
  }

  // 工作区（§6.3 workspace 扩展）：key = workspace_id（UUID v4）。
  const wm = asMap(root.get('workspaces'));
  if (wm) {
    wm.forEach((v, id) => {
      const sm = asMap(v);
      workspaces.push({
        id,
        name: getStr(sm, 'name') || '',
        cwd: getStr(sm, 'cwd') || '',
        createdAt: getStr(sm, 'created_at'),
        updatedAt: getStr(sm, 'updated_at'),
      });
    });
    // 排序：最近创建在前。
    workspaces.sort((a, b) =>
      String(b.createdAt || '').localeCompare(String(a.createdAt || '')),
    );
  }

  const g = asMap(root.get('global'));
  if (g) {
    globalStatus = getStr(g, 'status') || 'unknown'; // healthy|degraded|restarting
  }

  const projectsMap = asMap(root.get('projects'));
  projectsMap?.forEach((v, id) => {
    const p = asMap(v);
    if (!p) return;
    projects.push({
      id,
      name: getStr(p, 'name') || id,
      cwd: getStr(p, 'cwd') || '',
      instanceId: getStr(p, 'instance_id') || '',
      createdAt: getStr(p, 'created_at'),
      updatedAt: getStr(p, 'updated_at'),
      archivedAt: getStr(p, 'archived_at'),
    });
  });
  projects.sort((a, b) => String(b.updatedAt || '').localeCompare(String(a.updatedAt || '')));

  const projectSessionsMap = asMap(root.get('project_sessions'));
  projectSessionsMap?.forEach((v, id) => {
    const s = asMap(v);
    if (!s) return;
    projectSessions.push({
      id,
      projectId: getStr(s, 'project_id') || '',
      acpSessionId: getStr(s, 'acp_session_id'),
      title: getStr(s, 'title') || '新对话',
      lifecycle: getStr(s, 'lifecycle') || 'pending',
      updatedAt: getStr(s, 'updated_at'),
      lastOpenedAt: getStr(s, 'last_opened_at'),
      activeChatId: getStr(s, 'active_chat_id'),
    });
  });
  projectSessions.sort((a, b) => String(b.lastOpenedAt || b.updatedAt || '').localeCompare(String(a.lastOpenedAt || a.updatedAt || '')));

  // ACP 会话（agent 磁盘历史，instance 级）：key = session_id。
  const smap = asMap(root.get('sessions'));
  if (smap) {
    // 兜底去重：按 sessionId 去重（孤儿 key/客户端 doc 累积的残留可能
    // 产生重复；server 侧全量同步自愈后此兜底保持渲染稳定）。
    const seen = new Set<string>();
    smap.forEach((v) => {
      const sm = asMap(v);
      if (!sm) return;
      const sessionId = getStr(sm, 'session_id') || '';
      if (!sessionId || seen.has(sessionId)) return;
      seen.add(sessionId);
      sessions.push({
        sessionId,
        title: getStr(sm, 'title'),
        status: getStr(sm, 'status'),
        updatedAt: getStr(sm, 'updated_at'),
        cwd: getStr(sm, 'cwd'),
      });
    });
    // 排序：最近更新在前。
    sessions.sort((a, b) =>
      String(b.updatedAt || '').localeCompare(String(a.updatedAt || '')),
    );
  }

  return {
    instances,
    chats,
    sessions,
    workspaces,
    projects,
    projectSessions,
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
  const referencedToolIds = new Set<string>();

  const readToolCall = (tcId: string, tcm: Y.Map<unknown> | null): ToolCallInfo => ({
    toolCallId: tcId,
    name: getStr(tcm, 'name'),
    status: getStr(tcm, 'status'),
    arguments: yValue(tcm?.get('arguments')),
    result: yValue(tcm?.get('result')),
    resultOmitted: tcm?.has('result_omitted') ? tcm.get('result_omitted') === true : null,
    resultBytes: getNum(tcm, 'result_bytes'),
    publicError: (() => {
      const error = asMap(tcm?.get('public_error'));
      return error ? { code: getStr(error, 'code'), message: getStr(error, 'message') } : null;
    })(),
    startedAt: getStr(tcm, 'started_at'),
    completedAt: getStr(tcm, 'completed_at'),
  });

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
            if (tcId) referencedToolIds.add(tcId);
            item.toolCalls.push(readToolCall(tcId || '', tcm));
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

  // Compatibility repair for snapshots written before tool blocks were projected. Attach only
  // exact turn-id matches to an assistant entry; never infer from text/title or map adjacency.
  // Stable sorting keeps Y.Map iteration order from changing the visual history.
  const assistantByTurn = new Map(
    out
      .filter((entry) => entry.role === 'assistant' && entry.turnId)
      .map((entry) => [entry.turnId as string, entry]),
  );
  const legacyOrphans: Array<{ id: string; turnId: string; startedAt: string; map: Y.Map<unknown> }> = [];
  toolCalls?.forEach((value, id) => {
    if (referencedToolIds.has(id)) return;
    const map = asMap(value);
    const turnId = getStr(map, 'turn_id');
    if (!map || !turnId || !assistantByTurn.has(turnId)) return;
    legacyOrphans.push({ id, turnId, startedAt: getStr(map, 'started_at') || '', map });
  });
  legacyOrphans
    .sort((a, b) => a.startedAt.localeCompare(b.startedAt) || a.id.localeCompare(b.id))
    .forEach((orphan) => assistantByTurn.get(orphan.turnId)?.toolCalls.push(readToolCall(orphan.id, orphan.map)));

  return {
    schemaVersion: root.get('schema_version'),
    projectionVersion: root.get('projection_version'),
    entries: out,
  };
}

// ── renderControl：对话头部 + 权限请求（session:{cid} 投影，M3 §3.3）─────

/** 根 Map：session / agent / pending_permissions（active turn 内嵌于
 *  `session` map，对齐 Chat/Session 双 Doc 参考结构；旧根级 `chat`/
 *  `active_turn` 键已随迁移移除）。
 *  权限条（allow/deny → permission/resolve）数据取自 pending_permissions。
 *  ACP 会话列表不在 session doc（instance 级数据 → Registry Doc sessions，
 *  §6.3）——经 renderRegistry 读取，不随 chat 切换销毁。 */
export function renderControl(doc: Y.Doc): ControlView {
  const root = doc.getMap<unknown>('root');
  const result: ControlView = {
    chat: null,
    agent: null,
    activeTurn: null,
    pendingPermissions: [],
  };

  // `session` map：会话元信息 + active turn 内嵌字段（对齐参考 Session Doc：
  // sessionId/title/status/activeTurnId/activeTurnStatus/activeTurnUpdatedAt）。
  const sess = asMap(root.get('session'));
  if (sess) {
    result.chat = {
      chatId: getStr(sess, 'session_id') || '',
      title: getStr(sess, 'title'),
      status: getStr(sess, 'status'),
      activeTurnId: getStr(sess, 'active_turn_id'),
      createdAt: getStr(sess, 'created_at'),
      updatedAt: getStr(sess, 'updated_at'),
    };
    const turnId = getStr(sess, 'active_turn_id');
    const turnStatus = getStr(sess, 'active_turn_status');
    if (turnId || turnStatus) {
      result.activeTurn = {
        turnId,
        turnStatus,
        updatedAt: getStr(sess, 'active_turn_updated_at'),
      };
    }
  }

  const agent = asMap(root.get('agent'));
  if (agent) {
    const caps = asArray(agent.get('capabilities'));
    result.agent = {
      instanceId: getStr(agent, 'instance_id'),
      sessionId: getStr(agent, 'acp_session_id') ?? getStr(agent, 'session_id'),
      status: getStr(agent, 'status'),
      lastActivityAt: getStr(agent, 'last_activity_at'),
      capabilities: caps ? caps.toArray() : [],
      model: getStr(agent, 'model'),
      effort: getStr(agent, 'effort'),
      contextWindow: getNum(agent, 'context_window'),
      contextUsed: getNum(agent, 'context_used'),
    };
  }

  const perms = asMap(root.get('pending_permissions'));
  if (perms) {
    perms.forEach((p) => {
      const pm = asMap(p);
      if (!pm) return;
      // The server retains resolved/expired records for CAS idempotency. This selector exposes
      // actionable requests only so history cannot strand the permission bar or its decision lock.
      if (getStr(pm, 'status') !== 'pending') return;
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
