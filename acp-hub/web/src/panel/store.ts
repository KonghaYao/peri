// acp-hub Web 面板 —— 装配与编排（SolidJS 响应式 store）。
//
// 流程（M3 方案 §4，移植自原 main.js）：
//   1. 连接 → auth（ws-client 首帧纪律）→ ysync.subscribe ["hub:registry"]
//      → 快照 + ready → UI 启用。
//   2. registry 渲染 → 左栏实例/对话。
//   3. 点击对话 → subscribe ["chat:{cid}","session:{cid}"] → 快照渲染历史
//      → 增量实时更新（yjs 流式）。
//   4. 发送消息 → chat/prompt（commandId 记入 pendingAcks）→ 输入框清空。
//      用户消息气泡依赖 agent 回显（server 单写者，本地不造假）。
//   5. create committed ack（带 chatId）→ 自动订阅 chat:{cid} 并选中。
//   6. 断线：4500/4501/4502 停止并提示；1011/1013 指数退避重连（ws-client），
//      重连后重放订阅（快照兜底）。
//
// token 解析优先级（M3 §4）：URL ?token= → sessionStorage（避免落盘明文）
// → 输入框粘贴。

import { createSignal } from 'solid-js';
import type { Setter } from 'solid-js';
import * as H from './lib/protocol';
import { DocStore, renderChat, renderControl, renderRegistry } from './lib/yjs';
import type { ChatEntry, ChatInfo, ControlView, InstanceInfo, SessionSummaryInfo, WorkspaceInfo } from './lib/yjs';
import { WsClient } from './lib/ws-client';
import type { ConnStatus, ConnDetail } from './lib/ws-client';

const TOKEN_KEY = 'acp-hub-token';
const ACK_TIMEOUT_MS = 30000;

// ── UI 信号（组件消费）─────────────────────────────────────────────────

export const [tokenInput, setTokenInput] = createSignal('');
export const [busy, setBusy] = createSignal(false);
export const [connState, setConnState] = createSignal<{ text: string; kind: string }>({
  text: '未连接',
  kind: '',
});
export const [heartbeatCount, setHeartbeatCount] = createSignal(0);
export const [globalStatus, setGlobalStatus] = createSignal('');
export const [subscribedDocs, setSubscribedDocs] = createSignal('—');
export const [ackLog, setAckLog] = createSignal<string[]>([]);
export const [errorLog, setErrorLog] = createSignal<string[]>([]);
export const [instances, setInstances] = createSignal<InstanceInfo[]>([]);
export const [chats, setChats] = createSignal<ChatInfo[]>([]);
export const [selectedCid, setSelectedCid] = createSignal<string | null>(null);
export const [chatEntries, setChatEntries] = createSignal<ChatEntry[]>([]);
export const [chatHead, setChatHead] = createSignal<ControlView | null>(null);
export const [permissions, setPermissions] = createSignal<ControlView['pendingPermissions']>([]);
/** ACP 会话（§6.3 按需查询）：**按对话隔离缓存**——切换对话时向 agent 侧
 *  发 session/list 查询（真实数据源），结果存 `chatId → 列表`；切回对话
 *  复用缓存，10s 定时刷新当前对话。不再依赖 Registry Doc sessions 投影。 */
export const [sessionsByChat, setSessionsByChat] = createSignal<Record<string, SessionSummaryInfo[]>>({});
/** 当前选中对话的会话列表（跟随当前对话展示；未选对话 → []）。 */
export const currentSessions = (): SessionSummaryInfo[] => {
  const cid = selectedCid();
  return cid ? sessionsByChat()[cid] ?? [] : [];
};
/** 工作区定义（Registry Doc workspaces，§6.3 workspace 扩展）。 */
export const [workspaces, setWorkspaces] = createSignal<WorkspaceInfo[]>([]);
/** 当前选中的工作区 id（null = 全部）：左栏对话/会话按此过滤。 */
export const [selectedWsId, setSelectedWsId] = createSignal<string | null>(null);
export const [chatStatusSignal, setChatStatusSignal] = createSignal<Record<string, string>>({});
export const [toasts, setToasts] = createSignal<{ id: number; msg: string }[]>([]);

// ── 内部状态 ────────────────────────────────────────────────────────────

const store = new DocStore(); // docId → Y.Doc
let ws: WsClient | null = null; // 当前 WsClient
let currentCid: string | null = null; // 选中对话（重连后恢复订阅）
let ready = false; // ready 门控：就绪后才发 action
const pendingAcks = new Map<string, { label: string; cb?: (ack: Ack) => void; timer: ReturnType<typeof setTimeout> }>();
// 每个 chat 只接受最近一次 session/list 响应，防止切换/load 后的旧响应
// 晚到并覆盖「当前」标记。
const latestSessionQueries = new Map<string, string>();

interface Ack {
  commandId?: string;
  status?: string;
  chatId?: string;
  turnId?: string;
  [key: string]: unknown;
}

interface ActionError {
  commandId?: string;
  code?: string;
  message?: string;
}

let toastSeq = 0;

// ── toast ───────────────────────────────────────────────────────────────

export function toast(msg: string): void {
  const id = ++toastSeq;
  setToasts((list) => [...list, { id, msg }]);
  setTimeout(() => {
    setToasts((list) => list.filter((t) => t.id !== id));
  }, 2500);
}

// ── token 解析 ──────────────────────────────────────────────────────────

function resolveToken(): string {
  const params = new URLSearchParams(window.location.search);
  const fromUrl = params.get('token');
  if (fromUrl) {
    // base64 token 含 `+`，URL query 中 `+` 被解码为空格——还原之。
    const fixed = fromUrl.replace(/ /g, '+');
    // URL token 是一次性入口：写入 sessionStorage 供刷新复用，随后从
    // URL 中清理（避免明文留在地址栏/历史）。
    sessionStorage.setItem(TOKEN_KEY, fixed);
    params.delete('token');
    const qs = params.toString();
    const next = window.location.pathname + (qs ? '?' + qs : '');
    window.history.replaceState(null, '', next);
    return fixed;
  }
  return sessionStorage.getItem(TOKEN_KEY) || '';
}

// ── 订阅集合：registry 常驻 + 当前对话（chat + control 双 doc）─────────

function desiredDocs(): string[] {
  const docs = [H.DOC_REGISTRY];
  if (currentCid) {
    docs.push(H.chatDoc(currentCid));
    docs.push(H.sessionDoc(currentCid));
  }
  return docs;
}

// 订阅重放（首次连接与断线重连共用；ysync.subscribe 幂等）。
function sendSubscribe(): void {
  if (!ws || !ws.send(H.subscribe(desiredDocs()))) {
    toast('连接未就绪，订阅失败');
  }
}

// ── 对话选择 ────────────────────────────────────────────────────────────

// 终态对话（ACP 进程已退出/用户关闭/崩溃）：只读历史，禁交互。
export function isTerminal(status: string | undefined): boolean {
  return status === 'ended' || status === 'closed' || status === 'crashed';
}

export function selectChat(cid: string): void {
  if (cid === currentCid) {
    querySessions(cid);
    return;
  }
  const previousCid = currentCid;
  if (previousCid && ws) {
    ws.send(H.unsubscribe([H.chatDoc(previousCid), H.sessionDoc(previousCid)]));
  }
  currentCid = cid;
  setSelectedCid(cid);
  sendSubscribe(); // unsubscribe/subscribe 按 WebSocket 顺序生效，快照到达后渲染
  // 对话切换：清空旧渲染，等快照到达后重新填充。ACP 会话（sessions）是
  // **按对话隔离**的（§6.3）：切对话 → 立即按需查询该对话的会话列表 +
  // 启动 10s 定时刷新（agent 侧真实数据源）。
  setChatEntries([]);
  setChatHead(null);
  setPermissions([]);
  setSubscribedDocs('—'); // 订阅清单等下一帧 ready 刷新（简单置空亦可）
  restartSessionPoll(cid);
}

// ── session/list 按需查询（§6.3）────────────────────────────────────────

let sessionTimer: ReturnType<typeof setInterval> | null = null;

/** 切换对话时调用：立即查询一次 + 10s 定时刷新当前对话的会话列表。
 *  定时器随对话切换重建（上一对话的刷新立即停止——会话按对话隔离）。 */
function restartSessionPoll(cid: string): void {
  if (sessionTimer) {
    clearInterval(sessionTimer);
    sessionTimer = null;
  }
  querySessions(cid);
  sessionTimer = setInterval(() => querySessions(cid), 10000);
}

/** 发 session/list 查询（无副作用；结果经 session_list 帧更新缓存）。 */
function querySessions(cid: string): void {
  if (!ws) return;
  const frame = H.sessionList(cid);
  latestSessionQueries.set(cid, frame.commandId);
  if (!ws.send(frame)) latestSessionQueries.delete(cid);
}

/** 手动刷新当前对话的会话列表（不重置定时器/订阅/渲染）。 */
export function refreshSessions(): void {
  if (currentCid) querySessions(currentCid);
}

// ── ack 表 ──────────────────────────────────────────────────────────────

// 发送 action 并登记 ack 回调；ready 前不发（server 会缓冲，但面板
// 以 ready 门控保证可预期）。
function sendAction(frame: ReturnType<typeof H.action>, label: string, cb?: (ack: Ack) => void): void {
  if (!ws || !ws.send(frame)) {
    toast(`连接未就绪，无法发送 ${label}`);
    return;
  }
  const timer = setTimeout(() => {
    pendingAcks.delete(frame.commandId);
    toast(`ack 超时（30s）: ${label} ${frame.commandId.slice(0, 8)}…`);
  }, ACK_TIMEOUT_MS);
  pendingAcks.set(frame.commandId, { label, cb, timer });
}

// ── 下行帧分发 ──────────────────────────────────────────────────────────

function onFrame(frame: Record<string, unknown>): void {
  switch (frame.t) {
    case 'ysync.update':
      store.applyUpdateFrame(frame as { doc: string; update: string });
      break;
    case 'action_ack':
      onAck(frame as Ack);
      break;
    case 'action_error':
      onActionError(frame as ActionError);
      break;
    case 'session_list': {
      // 按需查询结果（§6.3）：更新该对话的会话缓存（按对话隔离）。
      const f = frame as { commandId?: string; chatId?: string; sessions?: SessionSummaryInfo[] };
      const chatId = f.chatId;
      if (chatId && f.commandId === latestSessionQueries.get(chatId)) {
        setSessionsByChat((prev) => ({ ...prev, [chatId]: f.sessions ?? [] }));
      }
      break;
    }
    default:
      break; // 未知帧忽略（协议演进兼容）
  }
}

function onAck(ack: Ack): void {
  showAck(ack);
  const pending = pendingAcks.get(ack.commandId || '');
  if (pending) {
    clearTimeout(pending.timer);
    pendingAcks.delete(ack.commandId || '');
    if (typeof pending.cb === 'function') pending.cb(ack);
  }
  // create 的 committed ack 携带 server 生成的 chatId —— 唯一告知
  // 路径：自动补订阅 chat:{cid}/session:{cid} 并选中。§8.5 激活语义：
  // 点击已打开的历史会话时 server 同样回 committed + 既有 chatId——
  // 此处统一切换选中（不新建重复对话）。
  if (ack.status === 'committed' && ack.chatId && ack.chatId !== currentCid) {
    toast(`已打开对话: ${ack.chatId.slice(0, 8)}…`);
    selectChat(ack.chatId);
  }
}

const ERROR_REASONS: Record<string, string> = {
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
};

function onActionError(err: ActionError): void {
  console.error('[panel] action 错误:', JSON.stringify(err));
  showActionError(err);
  const pending = pendingAcks.get(err.commandId || '');
  if (pending) {
    clearTimeout(pending.timer);
    pendingAcks.delete(err.commandId || '');
  }
  const reason = err.code ? ERROR_REASONS[err.code] : undefined;
  toast(
    (err.code || 'ACTION_ERROR') + (reason ? `：${reason}` : '') +
    (err.message ? `（${err.message}）` : ''),
  );
}

// ── 渲染入口（store.onUpdate：rAF 合帧后每个被更新 doc 调一次）──────────

store.onUpdate = (docId: string): void => {
  if (docId === H.DOC_REGISTRY) {
    const reg = renderRegistry(store.docFor(docId));
    // 状态映射（终态判定）：selectChat 需要当前 status。
    const statusMap: Record<string, string> = {};
    reg.chats.forEach((s) => {
      statusMap[s.id] = s.status || '';
    });
    setChatStatusSignal(statusMap);
    setInstances(reg.instances);
    setChats(reg.chats);
    setGlobalStatus(reg.globalStatus);
    // 工作区定义（§6.3 workspace 扩展）：左栏过滤依据。
    setWorkspaces(reg.workspaces);
    // ACP 会话列表不再取 Registry Doc sessions 投影（§6.3 按需查询）：
    // 切换对话时向 agent 侧发 session/list，结果按对话缓存（sessionsByChat）。
    // server 轮询投影保留（不破坏既有消费方），前端展示以按需查询为准。
    return;
  }
  if (currentCid && docId === H.chatDoc(currentCid)) {
    const conv = renderChat(store.docFor(docId));
    setChatEntries(conv.entries);
    return;
  }
  if (currentCid && docId === H.sessionDoc(currentCid)) {
    const ctrl = renderControl(store.docFor(docId));
    setChatHead(ctrl);
    setPermissions(ctrl.pendingPermissions);
  }
};

// ── 连接状态机回调（ws-client）─────────────────────────────────────────

function onStatus(state: ConnStatus, detail: ConnDetail): void {
  switch (state) {
    case 'connecting':
      ready = false;
      setConnState({ text: '连接中…', kind: '' });
      break;
    case 'open':
      // 已发 auth；认证后首帧必须是 ysync.subscribe 或 action ——
      // 立即重放订阅（首次连接与重连同一路径，快照兜底）。
      ready = false;
      setConnState({ text: '已认证', kind: 'ok' });
      sendSubscribe();
      break;
    case 'ready':
      ready = true;
      setConnState({ text: '就绪', kind: 'ok' });
      setSubscribedDocs(
        detail.projectionVersions
          ? Object.keys(detail.projectionVersions as Record<string, unknown>).join('、')
          : '—',
      );
      // 重连后恢复当前对话的会话轮询（若之前有选中对话）。
      if (currentCid) restartSessionPoll(currentCid);
      toast('连接就绪');
      break;
    case 'heartbeat':
      setHeartbeatCount((c) => c + 1);
      break;
    case 'reconnecting':
      setConnState({
        text: `重连中（${Math.round((detail.retryMs || 0) / 1000)}s 后）`,
        kind: 'warn',
      });
      break;
    case 'fatal':
      ready = false;
      if (sessionTimer) {
        clearInterval(sessionTimer);
        sessionTimer = null;
      }
      // connect() 置 busy(true) 后无任何路径恢复（closed 仅由用户主动
      // disconnect 触发，那里已 setBusy(false)）→ 必须在此恢复按钮，
      // 否则 4500/4501/4502 后 connect/disconnect 双双 disabled 无法重连。
      setBusy(false);
      setConnState({ text: `已停止（${detail.code}）`, kind: 'err' });
      toast(
        `连接终止（${detail.code}）：` +
        (detail.code === 4500 ? '实例离线' :
          detail.code === 4501 ? '心跳超时' :
          detail.code === 4502 ? '认证失败/配置性失败' : '未知原因') +
        '，不自动重连',
      );
      break;
    case 'closed':
      ready = false;
      setConnState({ text: '已断开', kind: '' });
      break;
    default:
      break;
  }
}

function wsUrl(): string {
  const scheme = window.location.protocol === 'https:' ? 'wss://' : 'ws://';
  return scheme + window.location.host + '/';
}

export function connect(token: string): void {
  if (!token) {
    toast('请先粘贴 token（或 ?token= 传入）');
    return;
  }
  sessionStorage.setItem(TOKEN_KEY, token);
  if (ws) {
    ws.close();
    ws = null;
  }
  pendingAcks.forEach((p) => clearTimeout(p.timer));
  pendingAcks.clear();
  ws = new WsClient({
    url: wsUrl(),
    token,
    onStatus,
    onFrame,
  });
  ws.connect();
  setBusy(true);
}

export function disconnect(): void {
  if (sessionTimer) {
    clearInterval(sessionTimer);
    sessionTimer = null;
  }
  if (ws) {
    ws.close();
    ws = null;
  }
  pendingAcks.forEach((p) => clearTimeout(p.timer));
  pendingAcks.clear();
  setBusy(false);
}

// ── 用户动作 → action ──────────────────────────────────────────────────

export function sendMessage(text: string, effort?: string): void {
  if (!ready) {
    toast('连接未就绪，稍后再试');
    return;
  }
  if (!currentCid) {
    toast('请先选择对话');
    return;
  }
  if (isTerminal(chatStatusSignal()[currentCid])) {
    toast('对话已结束，不能发送消息');
    return;
  }
  // 输入框已清空；用户消息气泡依赖 agent 回显（server 单写者，
  // 本地不造假，保证多标签页一致性）。effort 透传推理强度（若有）。
  sendAction(H.prompt(currentCid, text, effort), 'prompt', (ack) => {
    if (ack.status === 'committed' && ack.turnId) {
      toast(`消息已提交，turn=${ack.turnId.slice(0, 8)}…`);
    }
  });
}

export function newChat(): void {
  if (!ready) {
    toast('连接未就绪，稍后再试');
    return;
  }
  // instanceId/cwd 留空 = 本机；选中 workspace 时携带 workspace_id →
  // server 继承其 cwd（§6.3 workspace 扩展）。
  const wsId = selectedWsId();
  sendAction(H.createChat(undefined, undefined, wsId || undefined), 'create', (ack) => {
    // chatId 已在 onAck 里统一处理（自动订阅选中）
    if (!ack.chatId) toast('create committed 缺少 chatId');
  });
}

/** 点击 ACP 历史会话（§8.5 会话切换）：会话是**进程内实体**——在当前
 *  对话（其 ACP 进程）内 load 目标历史会话，**不新建对话/进程**。
 *  需先选中一个活跃对话（终态对话的进程已退出，无法切换）。 */
export function openAcpSession(acpSessionId: string, title?: string): void {
  if (!ready) {
    toast('连接未就绪，稍后再试');
    return;
  }
  const cid = currentCid;
  if (!cid) {
    toast('请先选择对话（会话在当前对话内切换）');
    return;
  }
  if (isTerminal(chatStatusSignal()[cid])) {
    toast('对话已结束，不能切换会话');
    return;
  }
  sendAction(H.loadChat(cid, acpSessionId), 'load', (ack) => {
    if (ack.status === 'committed') {
      toast(`已切换到会话 ${title || acpSessionId.slice(0, 8)}…`);
      // 会话列表的「当前」标记已变化 → 重新查询一次。
      querySessions(cid);
    }
  });
}

/** 当前对话内新建 ACP 会话（chat/session-new，§8.5）：会话是**进程内实体**——
 *  不新建对话/进程。committed 后刷新会话列表（tooltip「当前」标记更新）；
 *  错误由 onActionError 统一 toast。 */
export function newSession(): void {
  if (!ready) {
    toast('连接未就绪，稍后再试');
    return;
  }
  const cid = currentCid;
  if (!cid) {
    toast('请先选择对话（新会话在当前对话内创建）');
    return;
  }
  if (isTerminal(chatStatusSignal()[cid])) {
    toast('对话已结束，不能新建会话');
    return;
  }
  sendAction(H.sessionNew(cid), 'session/new', (ack) => {
    if (ack.status === 'committed') {
      toast('新会话已创建');
      // 会话列表的「当前」标记已变化 → 重新查询一次。
      querySessions(cid);
    }
  });
}

/** 新建工作区（§6.3 workspace 扩展）：定义本地目录 cwd，其下新建对话继承。
 *  cwd 须为已存在的绝对路径（server 校验：形态 + 目录存在性）。 */
export function createWorkspace(name: string, cwd: string): void {
  if (!ready) {
    toast('连接未就绪，稍后再试');
    return;
  }
  sendAction(H.workspaceCreate(name, cwd), 'workspace/create', (ack) => {
    if (ack.status === 'committed') {
      toast('工作区已创建');
    }
  });
}

/** 删除工作区定义（不影响已建对话/会话）。 */
export function removeWorkspace(workspaceId: string): void {
  if (!ready) {
    toast('连接未就绪，稍后再试');
    return;
  }
  sendAction(H.workspaceRemove(workspaceId), 'workspace/remove', (ack) => {
    if (ack.status === 'committed') {
      toast('工作区已删除');
      if (selectedWsId() === workspaceId) setSelectedWsId(null);
    }
  });
}

export function cancelTurn(): void {
  if (!ready) {
    toast('连接未就绪，稍后再试');
    return;
  }
  if (!currentCid) return;
  if (isTerminal(chatStatusSignal()[currentCid])) {
    toast('对话已结束，无需取消');
    return;
  }
  sendAction(H.cancel(currentCid), 'cancel');
}

export function closeChat(): void {
  if (!ready) {
    toast('连接未就绪，稍后再试');
    return;
  }
  if (!currentCid) return;
  if (isTerminal(chatStatusSignal()[currentCid])) {
    toast('对话已结束，无需关闭');
    return;
  }
  sendAction(H.close(currentCid), 'close');
}

export function resolvePermission(permissionId: string, decision: string): void {
  if (!ready) {
    toast('连接未就绪，稍后再试');
    return;
  }
  if (!currentCid) return;
  sendAction(H.resolvePermission(currentCid, permissionId, decision), 'resolve');
}

// ── 日志（最近 ack / 最近错误，最多保留 5 条）──────────────────────────

function pushLog(setter: Setter<string[]>, text: string): void {
  setter((list) => [...list, text].slice(-5));
}

function showAck(ack: Ack): void {
  const cid = ack.commandId || '';
  const short = cid.length > 8 ? cid.slice(0, 8) + '…' : cid;
  let line = `${short} → ${ack.status}`;
  if (ack.chatId) line += ` · cid=${ack.chatId.slice(0, 8)}…`;
  if (ack.turnId) line += ` · turn=${ack.turnId.slice(0, 8)}…`;
  pushLog(setAckLog, line);
}

function showActionError(err: ActionError): void {
  const cid = err.commandId || '';
  const short = cid.length > 8 ? cid.slice(0, 8) + '…' : cid;
  pushLog(setErrorLog, `${short} · ${err.code}${err.message ? ` · ${err.message}` : ''}`);
}

// ── 装配 ────────────────────────────────────────────────────────────────

const initialToken = resolveToken();
setTokenInput(initialToken);
