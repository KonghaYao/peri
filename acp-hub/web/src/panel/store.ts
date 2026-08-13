// acp-hub Web 面板 —— 装配与编排（SolidJS 响应式 store）。
//
// 流程（M3 方案 §4，移植自原 main.js）：
//   1. HttpOnly Cookie 鉴权 → ysync.subscribe ["hub:registry"]
//      → 快照 + ready → UI 启用。
//   2. registry 渲染 → 左栏实例/对话。
//   3. 点击对话 → subscribe ["chat:{cid}","session:{cid}"] → 快照渲染历史
//      → 增量实时更新（yjs 流式）。
//   4. 发送消息 → chat/prompt（commandId 记入 pendingAcks）→ 输入框清空。
//      用户消息气泡依赖 agent 回显（server 单写者，本地不造假）。
//   5. create committed ack（带 chatId）→ 自动订阅 chat:{cid} 并选中。
//   6. 断线：4500/4501/4502 停止并提示；1011/1013 指数退避重连（ws-client），
//      重连后重放订阅（快照兜底）。

import { createSignal } from 'solid-js';
import type { Setter } from 'solid-js';
import * as H from './lib/protocol';
import { DocStore, renderChat, renderControl, renderRegistry } from './lib/yjs';
import type { ChatEntry, ChatInfo, ControlView, ProjectInfo, ProjectSessionInfo, SessionSummaryInfo } from './lib/yjs';
import { WsClient } from './lib/ws-client';
import type { ConnStatus, ConnDetail } from './lib/ws-client';
import { canMutate, type PrincipalRole } from './lib/auth-role';
import { beginOpen, matchesOpening, shouldIgnoreLateAck, terminalCanCommit } from './lib/open-state.mjs';
import { unimportedSessions } from './lib/session-import.mjs';

const ACK_TIMEOUT_MS = 30000;

// ── UI 信号（组件消费）─────────────────────────────────────────────────

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
export const [chats, setChats] = createSignal<ChatInfo[]>([]);
export const [selectedCid, setSelectedCid] = createSignal<string | null>(null);
export const [chatEntries, setChatEntries] = createSignal<ChatEntry[]>([]);
export const [chatHead, setChatHead] = createSignal<ControlView | null>(null);
export const [permissions, setPermissions] = createSignal<ControlView['pendingPermissions']>([]);
export const [projects, setProjects] = createSignal<ProjectInfo[]>([]);
export const [projectSessions, setProjectSessions] = createSignal<ProjectSessionInfo[]>([]);
export const [importableSessions, setImportableSessions] = createSignal<SessionSummaryInfo[]>([]);
export const [selectedSessionId, setSelectedSessionId] = createSignal<string | null>(null);
export interface OpeningSession { commandId: string; sessionId: string; previousSessionId: string | null; previousChatId: string | null }
export const [openingSession, setOpeningSession] = createSignal<OpeningSession | null>(null);
export const openingSessionId = () => openingSession()?.sessionId ?? null;
export const [principalRole, setPrincipalRole] = createSignal<PrincipalRole>(null);
export const readOnly = () => !canMutate(principalRole());
export const [authInvalidated, setAuthInvalidated] = createSignal(0);
/** 当前选中的工作区 id（null = 全部）：左栏对话/会话按此过滤。 */
export const [chatStatusSignal, setChatStatusSignal] = createSignal<Record<string, string>>({});
export const [toasts, setToasts] = createSignal<{ id: number; msg: string }[]>([]);

// ── 内部状态 ────────────────────────────────────────────────────────────

const store = new DocStore(); // docId → Y.Doc
let ws: WsClient | null = null; // 当前 WsClient
let currentCid: string | null = null; // 选中对话（重连后恢复订阅）
let ready = false; // ready 门控：就绪后才发 action
const pendingAcks = new Map<string, { label: string; cb?: (ack: Ack) => void; timer: ReturnType<typeof setTimeout> }>();
const ignoredOpenCommands = new Set<string>();

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
  if (cid === currentCid) return;
  const previousCid = currentCid;
  if (previousCid && ws) {
    ws.send(H.unsubscribe([H.chatDoc(previousCid), H.sessionDoc(previousCid)]));
  }
  currentCid = cid;
  setSelectedCid(cid);
  sendSubscribe(); // unsubscribe/subscribe 按 WebSocket 顺序生效，快照到达后渲染
  // Runtime docs switch only after the logical session activation commits.
  setChatEntries([]);
  setChatHead(null);
  setPermissions([]);
  setSubscribedDocs('—'); // 订阅清单等下一帧 ready 刷新（简单置空亦可）
}

// ── ack 表 ──────────────────────────────────────────────────────────────

// 发送 action 并登记 ack 回调；ready 前不发（server 会缓冲，但面板
// 以 ready 门控保证可预期）。
function sendAction(frame: ReturnType<typeof H.action>, label: string, cb?: (ack: Ack) => void): boolean {
  if (!ws || !ws.send(frame)) {
    toast(`连接未就绪，无法发送 ${label}`);
    if (matchesOpening(openingSession(), frame.commandId)) setOpeningSession(null);
    return false;
  }
  const timer = setTimeout(() => {
    pendingAcks.delete(frame.commandId);
    if (matchesOpening(openingSession(), frame.commandId)) { ignoredOpenCommands.add(frame.commandId); setOpeningSession(null); }
    toast(`ack 超时（30s）: ${label} ${frame.commandId.slice(0, 8)}…`);
  }, ACK_TIMEOUT_MS);
  pendingAcks.set(frame.commandId, { label, cb, timer });
  return true;
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
    default:
      break; // 未知帧忽略（协议演进兼容）
  }
}

function onAck(ack: Ack): void {
  if (shouldIgnoreLateAck(ignoredOpenCommands, ack)) return;
  showAck(ack);
  const pending = pendingAcks.get(ack.commandId || '');
  // accepted is only the queue acknowledgement. Keep the command registered so
  // the committed/duplicate terminal acknowledgement can drive navigation.
  if (pending && ack.status !== 'accepted') {
    clearTimeout(pending.timer);
    pendingAcks.delete(ack.commandId || '');
    if (typeof pending.cb === 'function') pending.cb(ack);
  }
  // create 的 committed ack 携带 server 生成的 chatId —— 唯一告知
  // 路径：自动补订阅 chat:{cid}/session:{cid} 并选中。§8.5 激活语义：
  // 点击已打开的历史会话时 server 同样回 committed + 既有 chatId——
  // 此处统一切换选中（不新建重复对话）。
  if ((ack.status === 'committed' || ack.status === 'duplicate') && ack.chatId && ack.chatId !== currentCid && openingSession()?.commandId !== ack.commandId && !ignoredOpenCommands.has(ack.commandId || '')) {
    toast(`已打开对话: ${ack.chatId.slice(0, 8)}…`);
    const chatId = ack.chatId;
    if (!chatId) return;
    selectChat(chatId);
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
  if (matchesOpening(openingSession(), err.commandId)) setOpeningSession(null);
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
    setChats(reg.chats);
    setGlobalStatus(reg.globalStatus);
    // 工作区定义（§6.3 workspace 扩展）：左栏过滤依据。
    setProjects(reg.projects);
    setProjectSessions(reg.projectSessions);
    setImportableSessions(unimportedSessions(reg.sessions, reg.projectSessions));
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
      setOpeningSession(null);
      // connect() 置 busy(true) 后无任何路径恢复（closed 仅由用户主动
      // disconnect 触发，那里已 setBusy(false)）→ 必须在此恢复按钮，
      // 否则 4500/4501/4502 后 connect/disconnect 双双 disabled 无法重连。
      setBusy(false);
      if (detail.code === 4502) {
        clearUiSession();
        setPrincipalRole(null);
        setAuthInvalidated((v) => v + 1);
      }
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
      setOpeningSession(null);
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

export function connectWithCookie(): void {
  if (ws) ws.close();
  pendingAcks.forEach((p) => clearTimeout(p.timer));
  pendingAcks.clear();
  ws = new WsClient({ url: wsUrl(), onStatus, onFrame });
  ws.connect();
  setBusy(true);
}

export function createProject(name: string, cwd: string): void {
  if (!ready || readOnly()) return toast(readOnly() ? '只读模式不能创建项目' : '连接未就绪');
  sendAction(H.projectCreate(name, cwd), 'project/create');
}

export function createProjectSession(projectId: string, title?: string): void {
  if (!ready || readOnly()) return toast(readOnly() ? '只读模式不能创建会话' : '连接未就绪');
  sendAction(H.persistedSessionCreate(projectId, title), 'session/create', (ack) => {
    if ((ack.status === 'committed' || ack.status === 'duplicate') && ack.sessionId) setSelectedSessionId(ack.sessionId as string);
  });
}

export function openProjectSession(sessionId: string): void {
  if (!ready || readOnly()) return toast(readOnly() ? '只读模式不能打开运行会话' : '连接未就绪');
  const frame = H.persistedSessionOpen(sessionId);
  setOpeningSession(beginOpen(frame.commandId, sessionId, selectedSessionId(), currentCid));
  sendAction(frame, 'session/open', (ack) => {
    if (!terminalCanCommit(openingSession(), ack)) return;
    setSelectedSessionId(sessionId);
    const committedChatId = ack.chatId;
    if (!committedChatId) return;
    selectChat(committedChatId);
    setOpeningSession(null);
  });
}

export function renameProjectSession(sessionId: string, name: string): void {
  if (!ready || readOnly() || !name.trim()) return;
  sendAction(H.persistedSessionRename(sessionId, name.trim()), 'session/rename');
}

export function importProjectSession(projectId: string, acpSessionId: string): void {
  if (!ready || readOnly()) return toast(readOnly() ? '只读模式不能导入会话' : '连接未就绪');
  sendAction(H.persistedSessionImport(projectId, acpSessionId), 'session/import', (ack) => {
    if ((ack.status === 'committed' || ack.status === 'duplicate') && ack.sessionId) {
      toast('会话已加入侧边栏');
    }
  });
}

export function disconnect(): void {
  setOpeningSession(null);
  if (ws) {
    ws.close();
    ws = null;
  }
  pendingAcks.forEach((p) => clearTimeout(p.timer));
  pendingAcks.clear();
  setBusy(false);
}

export function clearUiSession(): void {
  disconnect();
  currentCid = null;
  setSelectedCid(null);
  setSelectedSessionId(null);
  setChatEntries([]);
  setChatHead(null);
  setPermissions([]);
  setProjects([]);
  setProjectSessions([]);
  setImportableSessions([]);
  ignoredOpenCommands.clear();
  store.clear();
}

export function selectPersistedSessionLocally(sessionId: string, chatId: string): void {
  setSelectedSessionId(sessionId); selectChat(chatId);
}

// ── 用户动作 → action ──────────────────────────────────────────────────

export function sendMessage(text: string, effort?: string): void {
  if (!ready || readOnly() || openingSessionId()) {
    if (readOnly()) return toast('只读模式不能发送消息');
    if (openingSessionId()) return toast('会话正在打开');
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

export function installPrincipalRole(role: PrincipalRole): void { setPrincipalRole(role); }

export function cancelTurn(): void {
  if (!ready || readOnly()) {
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
  if (!ready || readOnly()) {
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
  if (!ready || readOnly()) {
    if (readOnly()) return toast('只读模式不能处理权限请求');
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

// Browser UI authenticates through AuthGate and an HttpOnly cookie. The legacy
// credential is recovered from URL or Web Storage here.
