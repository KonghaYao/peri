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
import { acceptMessageSubmission, beginMessageSubmission, beginQuickStart, completesMessageSubmission, failMessageSubmission, isTurnActive, lockPermissionDecision, markMessageUncertain, quickStartCanActivate, unlockPermissionDecision, updateQuickStart } from './lib/action-state.mjs';
import { chooseRestorableSession, connectionProblemForClose, retainLiveRuntimeHints } from './lib/recovery-state.mjs';

const ACK_TIMEOUT_MS = 30000;

// ── UI 信号（组件消费）─────────────────────────────────────────────────

export const [busy, setBusy] = createSignal(false);
export const [connState, setConnState] = createSignal<{ text: string; kind: 'idle' | 'ok' | 'warn' | 'err' }>({
  text: '未连接',
  kind: 'idle',
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
export interface PersistentError { id: number; title: string; detail: string; commandId: string | null; retryable: boolean; retrying: boolean }
export const [persistentErrors, setPersistentErrors] = createSignal<PersistentError[]>([]);
export interface MessageSubmission { commandId: string; text: string; phase: 'sending' | 'accepted' | 'uncertain' | 'failed'; detail: string | null; retryable: boolean }
export const [messageSubmission, setMessageSubmission] = createSignal<MessageSubmission | null>(null);
export const turnActive = () => isTurnActive(chatHead()?.activeTurn);
export const [cancellingTurn, setCancellingTurn] = createSignal(false);
export const [closingChat, setClosingChat] = createSignal(false);
export const [pendingPermissionDecisions, setPendingPermissionDecisions] = createSignal<Map<string, string>>(new Map());
export interface ConnectionProblem { code: number | null; title: string; detail: string; action: 'reconnect' | 'login' | null }
export const [connectionProblem, setConnectionProblem] = createSignal<ConnectionProblem | null>(null);
export const [restoringSessionId, setRestoringSessionId] = createSignal<string | null>(null);
export interface QuickStartSubmission { commandId: string; projectId: string; text: string; phase: 'creating' | 'accepted' | 'uncertain' | 'failed'; detail: string | null; retryable: boolean }
export const [quickStartSubmission, setQuickStartSubmission] = createSignal<QuickStartSubmission | null>(null);
export const [creatingSessionProjectId, setCreatingSessionProjectId] = createSignal<string | null>(null);

// ── 内部状态 ────────────────────────────────────────────────────────────

const store = new DocStore(); // docId → Y.Doc
let ws: WsClient | null = null; // 当前 WsClient
let connectionEpoch = 0; // 隔离被替换连接的延迟 status/frame 回调
let currentCid: string | null = null; // 选中对话（重连后恢复订阅）
let ready = false; // ready 门控：就绪后才发 action
interface PendingAction {
  label: string;
  frame: ActionFrame;
  options: ActionOptions;
  cb?: (ack: Ack) => void;
  onAccepted?: (ack: Ack) => void;
  onTimeout?: () => void;
  onError?: (err: ActionError) => void;
  retryOnUncertain?: boolean;
  timer: ReturnType<typeof setTimeout>;
}
type ActionFrame = ReturnType<typeof H.action>;
type ActionOptions = Omit<PendingAction, 'label' | 'frame' | 'options' | 'timer'>;
const pendingAcks = new Map<string, PendingAction>();
const uncertainActionRetries = new Map<string, { frame: ActionFrame; label: string; options: ActionOptions }>();
const [uncertainMetadataCount, setUncertainMetadataCount] = createSignal(0);
const ignoredOpenCommands = new Set<string>();
let retryableMessageFrame: ReturnType<typeof H.action> | null = null;
let retryableQuickStartFrame: ReturnType<typeof H.action> | null = null;
const permissionCommands = new Map<string, string>();
const LAST_SESSION_KEY = 'acp-hub:last-session';
let registryReceived = false;
let attemptedRestore = false;

function registerUncertainAction(commandId: string, retry: { frame: ActionFrame; label: string; options: ActionOptions }): void {
  uncertainActionRetries.set(commandId, retry);
  setUncertainMetadataCount(uncertainActionRetries.size);
}

function forgetUncertainAction(commandId: string): void {
  uncertainActionRetries.delete(commandId);
  setUncertainMetadataCount(uncertainActionRetries.size);
}

function clearUncertainActions(): void {
  uncertainActionRetries.clear();
  setUncertainMetadataCount(0);
}

function rejectWhileMetadataUncertain(): boolean {
  if (!uncertainMetadataCount()) return false;
  toast('先处理“结果尚未确认”的项目或会话操作');
  return true;
}

function settlePendingActionsAsUncertain(): void {
  for (const [commandId, pending] of [...pendingAcks]) {
    clearTimeout(pending.timer);
    pendingAcks.delete(commandId);
    if (matchesOpening(openingSession(), commandId)) {
      ignoredOpenCommands.add(commandId);
      setOpeningSession(null);
    }
    if (pending.retryOnUncertain) registerUncertainAction(commandId, {
      frame: pending.frame, label: pending.label, options: pending.options,
    });
    if (pending.onTimeout) pending.onTimeout();
    else persistActionProblem('结果尚未确认', `${pending.label} 在连接中断前未收到终态回复。服务器可能仍会完成操作，请先等待状态同步，不要盲目重复执行。`, commandId);
  }
}

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
  retryable?: boolean;
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

export function dismissPersistentError(id: number): void {
  setPersistentErrors((items) => {
    const target = items.find((item) => item.id === id);
    if (target?.commandId) forgetUncertainAction(target.commandId);
    return items.filter((item) => item.id !== id);
  });
}

export function retainPersistentErrors(items: PersistentError[], next: PersistentError): PersistentError[] {
  const merged = [...items.filter((item) => !next.commandId || item.commandId !== next.commandId), next];
  const protectedErrors = merged.filter((item) => item.retryable || item.retrying);
  const ordinarySlots = Math.max(0, 5 - protectedErrors.length);
  const retainedOrdinaryIds = new Set((ordinarySlots === 0 ? [] : merged
    .filter((item) => !item.retryable && !item.retrying)
    .slice(-ordinarySlots))
    .map((item) => item.id));
  return merged.filter((item) => item.retryable || item.retrying || retainedOrdinaryIds.has(item.id));
}

function persistActionProblem(title: string, detail: string, commandId?: string): void {
  setPersistentErrors((items) => retainPersistentErrors(items, {
    id: ++toastSeq, title, detail, commandId: commandId || null, retryable: !!commandId && uncertainActionRetries.has(commandId), retrying: false,
  }));
}

export function retryPersistentAction(commandId: string): boolean {
  const retry = uncertainActionRetries.get(commandId);
  if (!retry || !ready) {
    toast(!retry ? '此操作不能安全重试' : '连接未就绪，暂时不能重新确认');
    return false;
  }
  if (pendingAcks.has(commandId)) return false;
  const sent = sendAction(retry.frame, retry.label, retry.options);
  if (sent) setPersistentErrors((items) => items.map((item) => item.commandId === commandId
    ? { ...item, title: '正在重新确认', detail: '正在使用原 commandId 查询或完成此操作，不会创建第二个请求。', retryable: false, retrying: true }
    : item));
  else persistActionProblem('重新确认尚未发送', '连接未就绪。原请求仍被保留，请恢复连接后再次确认。', commandId);
  return sent;
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
function sendAction(frame: ActionFrame, label: string, options: ActionOptions = {}): boolean {
  if (!ws || !ws.send(frame)) {
    toast(`连接未就绪，无法发送 ${label}`);
    options.onError?.({ commandId: frame.commandId, code: 'UNAVAILABLE', message: '连接未就绪', retryable: true });
    if (matchesOpening(openingSession(), frame.commandId)) setOpeningSession(null);
    return false;
  }
  const timer = setTimeout(() => {
    pendingAcks.delete(frame.commandId);
    if (matchesOpening(openingSession(), frame.commandId)) { ignoredOpenCommands.add(frame.commandId); setOpeningSession(null); }
    if (options.retryOnUncertain) registerUncertainAction(frame.commandId, { frame, label, options });
    options.onTimeout?.();
    if (!options.onTimeout) persistActionProblem('结果尚未确认', `${label} 已超过 30 秒未收到终态回复。服务器可能仍会完成操作，请先等待状态同步，不要盲目重复执行。`, frame.commandId);
  }, ACK_TIMEOUT_MS);
  pendingAcks.set(frame.commandId, { label, ...options, frame, options, timer });
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
    case 'auth_error':
      setConnectionProblem({ code: 4502, title: '登录状态已失效', detail: '访问令牌已失效、被撤销或服务器已重启，需要重新登录。', action: 'login' });
      clearUiSession();
      setPrincipalRole(null);
      setAuthInvalidated((value) => value + 1);
      break;
    default:
      break; // 未知帧忽略（协议演进兼容）
  }
}

function onAck(ack: Ack): void {
  if (shouldIgnoreLateAck(ignoredOpenCommands, ack)) return;
  showAck(ack);
  if (completesMessageSubmission(messageSubmission(), ack)) {
    setMessageSubmission(null);
    retryableMessageFrame = null;
  }
  if (quickStartCanActivate(quickStartSubmission(), ack)) {
    const quick = quickStartSubmission()!;
    const sessionId = ack.sessionId as string;
    const chatId = ack.chatId as string;
    setQuickStartSubmission(null);
    retryableQuickStartFrame = null;
    setSelectedSessionId(sessionId);
    rememberSession(sessionId);
    selectChat(chatId);
    if (!sendMessage(quick.text)) persistActionProblem('首条消息尚未发送', `会话已经创建，但消息未提交。请复制以下原文后重新发送：\n\n${quick.text}`, quick.commandId);
  }
  if (ack.status !== 'accepted' && ack.commandId) {
    permissionCommands.delete(ack.commandId);
    forgetUncertainAction(ack.commandId);
    setPersistentErrors((items) => items.filter((item) => item.commandId !== ack.commandId));
  }
  const pending = pendingAcks.get(ack.commandId || '');
  // accepted is only the queue acknowledgement. Keep the command registered so
  // the committed/duplicate terminal acknowledgement can drive navigation.
  if (pending && ack.status === 'accepted') pending.onAccepted?.(ack);
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
  console.error(`[panel] action 错误 code=${err.code || 'UNKNOWN'} command=${err.commandId ? 'present' : 'absent'}`);
  showActionError(err);
  const submission = messageSubmission();
  if (submission && submission.commandId === err.commandId) {
    setMessageSubmission(failMessageSubmission(submission, submission.commandId, err.message || '消息提交失败', !!err.retryable));
  }
  if (err.commandId) {
    forgetUncertainAction(err.commandId);
    const permissionId = permissionCommands.get(err.commandId);
    if (permissionId) {
      setPendingPermissionDecisions((decisions) => unlockPermissionDecision(decisions, permissionId));
      permissionCommands.delete(err.commandId);
    }
  }
  const pending = pendingAcks.get(err.commandId || '');
  if (pending) {
    clearTimeout(pending.timer);
    pendingAcks.delete(err.commandId || '');
    pending.onError?.(err);
  }
  if (matchesOpening(openingSession(), err.commandId)) setOpeningSession(null);
  const reason = err.code ? ERROR_REASONS[err.code] : undefined;
  persistActionProblem(reason || err.code || '操作失败', err.message || '服务器未提供更多信息。', err.commandId);
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
    const projectedSessions = retainLiveRuntimeHints(reg.projectSessions, reg.chats) as ProjectSessionInfo[];
    setProjectSessions(projectedSessions);
    setImportableSessions(unimportedSessions(reg.sessions, reg.projectSessions));
    registryReceived = true;
    restoreLastSession(projectedSessions);
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
    const visiblePermissionIds = new Set(ctrl.pendingPermissions.map((item) => item.permissionId).filter((id): id is string => !!id));
    setPendingPermissionDecisions((decisions) => new Map([...decisions].filter(([id]) => visiblePermissionIds.has(id))));
  }
};

// ── 连接状态机回调（ws-client）─────────────────────────────────────────

function onStatus(state: ConnStatus, detail: ConnDetail): void {
  switch (state) {
    case 'connecting':
      ready = false;
      setConnState({ text: '连接中…', kind: 'idle' });
      setConnectionProblem(null);
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
      setConnectionProblem(null);
      if (registryReceived) restoreLastSession(projectSessions());
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
      settlePendingActionsAsUncertain();
      setMessageSubmission((current) => current ? markMessageUncertain(current, current.commandId) : null);
      setQuickStartSubmission((current) => current ? updateQuickStart(current, current.commandId, 'uncertain', '连接中断，创建结果尚未确认。重新确认会复用同一请求。') : null);
      setConnState({
        text: `重连中（${Math.round((detail.retryMs || 0) / 1000)}s 后）`,
        kind: 'warn',
      });
      break;
    case 'fatal':
      ready = false;
      settlePendingActionsAsUncertain();
      setOpeningSession(null);
      setMessageSubmission((current) => current ? markMessageUncertain(current, current.commandId) : null);
      setQuickStartSubmission((current) => current ? updateQuickStart(current, current.commandId, 'uncertain', '连接已停止，创建结果尚未确认。重新登录或连接后可安全确认。') : null);
      // connect() 置 busy(true) 后无任何路径恢复（closed 仅由用户主动
      // disconnect 触发，那里已 setBusy(false)）→ 必须在此恢复按钮，
      // 否则 4500/4501/4502 后 connect/disconnect 双双 disabled 无法重连。
      setBusy(false);
      if (detail.code === 4502) {
        clearUiSession();
        setPrincipalRole(null);
        setAuthInvalidated((v) => v + 1);
      }
      setConnectionProblem(connectionProblemForClose(detail.code));
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
      settlePendingActionsAsUncertain();
      setOpeningSession(null);
      setMessageSubmission((current) => current ? markMessageUncertain(current, current.commandId) : null);
      setQuickStartSubmission((current) => current ? updateQuickStart(current, current.commandId, 'uncertain', '连接已断开，创建结果尚未确认。重新连接后可安全确认。') : null);
      setConnState({ text: '已断开', kind: 'idle' });
      if (principalRole()) setConnectionProblem({ code: null, title: '连接已断开', detail: '当前页面没有连接到 acp-hub server。你的持久会话仍然安全。', action: 'reconnect' });
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
  settlePendingActionsAsUncertain();
  const epoch = ++connectionEpoch;
  if (ws) ws.close();
  const client = new WsClient({
    url: wsUrl(),
    onStatus: (state, detail) => { if (epoch === connectionEpoch) onStatus(state, detail); },
    onFrame: (frame) => { if (epoch === connectionEpoch) onFrame(frame); },
  });
  ws = client;
  client.connect();
  setBusy(true);
  setConnectionProblem(null);
}

export function reconnect(): void { connectWithCookie(); }

function rememberSession(sessionId: string): void {
  try { window.localStorage.setItem(LAST_SESSION_KEY, sessionId); } catch { /* 浏览器禁用存储时退化为手动选择 */ }
}

function restoreLastSession(sessions: ProjectSessionInfo[]): void {
  if (!ready || attemptedRestore || selectedSessionId() || openingSession()) return;
  attemptedRestore = true;
  let preferred: string | null = null;
  try { preferred = window.localStorage.getItem(LAST_SESSION_KEY); } catch { /* 手动选择兜底 */ }
  const candidate = chooseRestorableSession(preferred, sessions) as ProjectSessionInfo | null;
  if (!candidate) {
    if (preferred) try { window.localStorage.removeItem(LAST_SESSION_KEY); } catch { /* stale preference is harmless */ }
    return;
  }
  setRestoringSessionId(candidate.id);
  if (readOnly() && candidate.activeChatId) {
    selectPersistedSessionLocally(candidate.id, candidate.activeChatId);
    setRestoringSessionId(null);
    return;
  }
  if (readOnly()) { setRestoringSessionId(null); return; }
  openProjectSession(candidate.id);
}

export function createProject(name: string, cwd: string, onCommitted?: () => void, onFailed?: () => void): boolean {
  if (!ready || readOnly() || rejectWhileMetadataUncertain()) {
    if (readOnly()) toast('只读模式不能创建项目'); else if (!ready) toast('连接未就绪');
    return false;
  }
  const frame = H.projectCreate(name, cwd);
  return sendAction(frame, 'project/create', { retryOnUncertain: true, cb: (ack) => {
    if (ack.status !== 'committed' && ack.status !== 'duplicate') return;
    toast('项目已创建');
    onCommitted?.();
  }, onError: () => onFailed?.(), onTimeout: () => {
    persistActionProblem('创建项目结果尚未确认', '项目可能已经创建。表单内容已保留，请先等待侧边栏同步，不要立即重复提交。', frame.commandId);
    onFailed?.();
  } });
}

export function archiveProject(projectId: string, onCommitted?: () => void, onFailed?: () => void): boolean {
  if (!ready || readOnly() || rejectWhileMetadataUncertain()) {
    if (readOnly()) toast('只读模式不能归档项目'); else if (!ready) toast('连接未就绪');
    return false;
  }
  const frame = H.projectArchive(projectId);
  return sendAction(frame, 'project/archive', { retryOnUncertain: true, cb: (ack) => {
    if (ack.status !== 'committed' && ack.status !== 'duplicate') return;
    const selected = projectSessions().find((session) => session.id === selectedSessionId());
    if (selected?.projectId === projectId) {
      setSelectedSessionId(null);
      const previousCid = currentCid;
      if (previousCid && ws) ws.send(H.unsubscribe([H.chatDoc(previousCid), H.sessionDoc(previousCid)]));
      currentCid = null;
      setSelectedCid(null);
      setChatEntries([]);
      setChatHead(null);
      setPermissions([]);
      try { window.localStorage.removeItem(LAST_SESSION_KEY); } catch { /* UI preference only */ }
    }
    toast('项目已归档');
    onCommitted?.();
  }, onError: () => onFailed?.(), onTimeout: () => {
    persistActionProblem('归档结果尚未确认', '项目可能已经归档。请等待侧边栏同步后再决定是否重试。', frame.commandId);
    onFailed?.();
  } });
}

export function restoreProject(projectId: string, onCommitted?: () => void, onFailed?: () => void): boolean {
  if (!ready || readOnly() || rejectWhileMetadataUncertain()) {
    if (readOnly()) toast('只读模式不能恢复项目'); else if (!ready) toast('连接未就绪');
    return false;
  }
  const frame = H.projectRestore(projectId);
  return sendAction(frame, 'project/restore', { retryOnUncertain: true, cb: (ack) => {
    if (ack.status !== 'committed' && ack.status !== 'duplicate') return;
    toast('项目已恢复');
    onCommitted?.();
  }, onError: () => onFailed?.(), onTimeout: () => {
    persistActionProblem('恢复项目结果尚未确认', '项目可能已经恢复。请等待侧边栏同步后再决定是否重试。', frame.commandId);
    onFailed?.();
  } });
}

export function renameProject(projectId: string, name: string, onCommitted?: () => void, onFailed?: () => void): boolean {
  if (!ready || readOnly() || !name.trim() || rejectWhileMetadataUncertain()) {
    if (readOnly()) toast('只读模式不能重命名项目'); else if (!ready) toast('连接未就绪');
    return false;
  }
  const frame = H.projectRename(projectId, name.trim());
  return sendAction(frame, 'project/rename', { retryOnUncertain: true, cb: (ack) => {
    if (ack.status !== 'committed' && ack.status !== 'duplicate') return;
    toast('项目已重命名');
    onCommitted?.();
  }, onError: () => onFailed?.(), onTimeout: () => {
    persistActionProblem('重命名项目结果尚未确认', '名称可能已经保存。输入内容已保留，请先等待侧边栏同步。', frame.commandId);
    onFailed?.();
  } });
}

export function createProjectSession(projectId: string, title?: string): boolean {
  if (!ready || readOnly() || creatingSessionProjectId() || quickStartSubmission() || rejectWhileMetadataUncertain()) {
    if (readOnly()) toast('只读模式不能创建会话'); else if (!ready) toast('连接未就绪'); else if (creatingSessionProjectId() || quickStartSubmission()) toast('已有会话正在创建');
    return false;
  }
  const frame = H.persistedSessionCreate(projectId, title);
  setCreatingSessionProjectId(projectId);
  const finish = () => setCreatingSessionProjectId(null);
  return sendAction(frame, 'session/create', { retryOnUncertain: true, cb: (ack) => {
    if (ack.status !== 'committed' && ack.status !== 'duplicate') return;
    finish();
    if ((ack.status === 'committed' || ack.status === 'duplicate') && ack.sessionId) {
      setSelectedSessionId(ack.sessionId as string);
      rememberSession(ack.sessionId as string);
    }
  }, onError: finish, onTimeout: () => {
    finish();
    persistActionProblem('创建会话结果尚未确认', '会话可能已经创建。请等待侧边栏同步，不要立即再次点击新建。', frame.commandId);
  } });
}

export function createSessionWithFirstMessage(projectId: string, text: string): boolean {
  const source = text.trim();
  if (!source || quickStartSubmission() || messageSubmission() || creatingSessionProjectId()) return false;
  if (!ready || readOnly() || rejectWhileMetadataUncertain()) {
    if (readOnly()) toast('只读模式不能创建会话'); else if (!ready) toast('连接未就绪');
    return false;
  }
  const title = source.split(/\r?\n/, 1)[0].slice(0, 60);
  const frame = H.persistedSessionCreate(projectId, title);
  retryableQuickStartFrame = frame;
  setQuickStartSubmission(beginQuickStart(frame.commandId, projectId, source) as QuickStartSubmission);
  const sent = sendAction(frame, 'session/create', {
    onAccepted: () => setQuickStartSubmission((current) => updateQuickStart(current, frame.commandId, 'accepted')),
    onTimeout: () => setQuickStartSubmission((current) => updateQuickStart(current, frame.commandId, 'uncertain', '会话创建结果尚未确认。重新确认会复用同一请求，不会创建重复会话。')),
    onError: (error) => setQuickStartSubmission((current) => updateQuickStart(current, frame.commandId, 'failed', error.message || '无法创建会话。你的消息仍保留。', !!error.retryable)),
  });
  if (!sent) setQuickStartSubmission((current) => updateQuickStart(current, frame.commandId, 'failed', '连接未就绪。你的消息仍保留。'));
  return sent;
}

export function retryQuickStart(): void {
  const current = quickStartSubmission();
  const frame = retryableQuickStartFrame;
  if (!current || !frame || current.commandId !== frame.commandId || !['uncertain', 'failed'].includes(current.phase)) return;
  if (!ready) return toast('连接未就绪，暂时不能重新确认');
  setQuickStartSubmission(updateQuickStart(current, frame.commandId, 'creating'));
  sendAction(frame, 'session/create', {
    onAccepted: () => setQuickStartSubmission((value) => updateQuickStart(value, frame.commandId, 'accepted')),
    onTimeout: () => setQuickStartSubmission((value) => updateQuickStart(value, frame.commandId, 'uncertain', '会话创建结果尚未确认。重新确认会复用同一请求，不会创建重复会话。')),
    onError: (error) => setQuickStartSubmission((value) => updateQuickStart(value, frame.commandId, 'failed', error.message || '无法创建会话。你的消息仍保留。', !!error.retryable)),
  });
}

export function dismissFailedQuickStart(): void {
  if (quickStartSubmission()?.phase !== 'failed') return;
  setQuickStartSubmission(null);
  retryableQuickStartFrame = null;
}

export interface OpenSessionCallbacks {
  onCommitted?: () => void;
  onFailed?: (message: string) => void;
  onUncertain?: () => void;
}

export function openProjectSession(sessionId: string, callbacks: OpenSessionCallbacks = {}): boolean {
  if (!ready || readOnly() || rejectWhileMetadataUncertain()) {
    const message = readOnly() ? '只读模式不能打开运行会话' : !ready ? '连接未就绪' : '先确认上一项操作';
    toast(message);
    callbacks.onFailed?.(message);
    return false;
  }
  const frame = H.persistedSessionOpen(sessionId);
  setOpeningSession(beginOpen(frame.commandId, sessionId, selectedSessionId(), currentCid));
  return sendAction(frame, 'session/open', { cb: (ack) => {
    if (!terminalCanCommit(openingSession(), ack)) return;
    setSelectedSessionId(sessionId);
    const committedChatId = ack.chatId;
    if (!committedChatId) return;
    selectChat(committedChatId);
    rememberSession(sessionId);
    setRestoringSessionId(null);
    setOpeningSession(null);
    callbacks.onCommitted?.();
  }, onError: (error) => { setRestoringSessionId(null); attemptedRestore = false; callbacks.onFailed?.(error.message || '无法打开会话'); }, onTimeout: () => { setRestoringSessionId(null); attemptedRestore = false; persistActionProblem('打开结果尚未确认', '未切换当前会话。请等待侧边栏状态同步；如果会话仍未启动，再重新打开。', frame.commandId); callbacks.onUncertain?.(); } });
}

export function renameProjectSession(sessionId: string, name: string, onCommitted?: () => void, onFailed?: () => void): boolean {
  if (!ready || readOnly() || !name.trim() || rejectWhileMetadataUncertain()) return false;
  const frame = H.persistedSessionRename(sessionId, name.trim());
  return sendAction(frame, 'session/rename', { retryOnUncertain: true, cb: (ack) => {
    if (ack.status !== 'committed' && ack.status !== 'duplicate') return;
    toast('会话已重命名');
    onCommitted?.();
  }, onError: () => onFailed?.(), onTimeout: () => {
    persistActionProblem('重命名会话结果尚未确认', '名称可能已经保存。输入内容已保留，请先等待侧边栏同步。', frame.commandId);
    onFailed?.();
  } });
}

export function importProjectSession(projectId: string, acpSessionId: string, onCommitted?: () => void, onFailed?: () => void): boolean {
  if (!ready || readOnly() || rejectWhileMetadataUncertain()) { toast(readOnly() ? '只读模式不能导入会话' : !ready ? '连接未就绪' : '先确认上一项操作'); return false; }
  const frame = H.persistedSessionImport(projectId, acpSessionId);
  return sendAction(frame, 'session/import', { retryOnUncertain: true, cb: (ack) => {
    if ((ack.status === 'committed' || ack.status === 'duplicate') && ack.sessionId) {
      toast('会话已加入侧边栏');
      onCommitted?.();
    }
  }, onError: () => onFailed?.(), onTimeout: () => { persistActionProblem('导入结果尚未确认', '服务器可能已导入此会话。请等待侧边栏刷新；如需确认，请使用原请求重试。', frame.commandId); onFailed?.(); } });
}

export function disconnect(): void {
  settlePendingActionsAsUncertain();
  connectionEpoch += 1;
  ready = false;
  setOpeningSession(null);
  setClosingChat(false);
  if (ws) {
    ws.close();
    ws = null;
  }
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
  setMessageSubmission(null);
  retryableMessageFrame = null;
  setQuickStartSubmission(null);
  retryableQuickStartFrame = null;
  setCreatingSessionProjectId(null);
  setPersistentErrors([]);
  clearUncertainActions();
  setCancellingTurn(false);
  setClosingChat(false);
  setPendingPermissionDecisions(new Map<string, string>());
  setConnectionProblem(null);
  setRestoringSessionId(null);
  permissionCommands.clear();
  registryReceived = false;
  attemptedRestore = false;
  ignoredOpenCommands.clear();
  store.clear();
}

export function selectPersistedSessionLocally(sessionId: string, chatId: string): void {
  setSelectedSessionId(sessionId); rememberSession(sessionId); selectChat(chatId);
}

// ── 用户动作 → action ──────────────────────────────────────────────────

export function sendMessage(text: string, effort?: string): boolean {
  if (!ready || readOnly() || openingSessionId() || turnActive() || messageSubmission()) {
    if (readOnly()) { toast('只读模式不能发送消息'); return false; }
    if (openingSessionId()) { toast('会话正在打开'); return false; }
    if (turnActive()) { toast('Agent 正在工作，可先停止当前任务'); return false; }
    if (messageSubmission()) { toast('上一条消息仍在确认中'); return false; }
    toast('连接未就绪，稍后再试');
    return false;
  }
  if (!currentCid) {
    toast('请先选择对话');
    return false;
  }
  if (isTerminal(chatStatusSignal()[currentCid])) {
    toast('对话已结束，不能发送消息');
    return false;
  }
  const frame = H.prompt(currentCid, text, effort);
  retryableMessageFrame = frame;
  setMessageSubmission(beginMessageSubmission(frame.commandId, text) as MessageSubmission);
  const sent = sendAction(frame, 'prompt', {
    onAccepted: () => setMessageSubmission((current) => acceptMessageSubmission(current, frame.commandId)),
    onTimeout: () => setMessageSubmission((current) => markMessageUncertain(current, frame.commandId)),
    onError: (err) => setMessageSubmission((current) => failMessageSubmission(current, frame.commandId, err.message || '消息提交失败', !!err.retryable)),
    cb: (ack) => {
      if (completesMessageSubmission(messageSubmission(), ack)) {
        setMessageSubmission(null);
        retryableMessageFrame = null;
      }
    },
  });
  if (!sent) {
    setMessageSubmission((current) => failMessageSubmission(current, frame.commandId, '连接未就绪，消息未发送', true));
  }
  return sent;
}

export function retryMessageSubmission(): void {
  const current = messageSubmission();
  const frame = retryableMessageFrame;
  if (!current || !frame || current.commandId !== frame.commandId) return;
  if (!ready) return toast('连接未就绪，暂时不能重新确认');
  setMessageSubmission({ ...current, phase: 'sending', detail: null });
  sendAction(frame, 'prompt', {
    onAccepted: () => setMessageSubmission((value) => acceptMessageSubmission(value, frame.commandId)),
    onTimeout: () => setMessageSubmission((value) => markMessageUncertain(value, frame.commandId)),
    onError: (err) => setMessageSubmission((value) => failMessageSubmission(value, frame.commandId, err.message || '消息提交失败', !!err.retryable)),
    cb: (ack) => {
      if (completesMessageSubmission(messageSubmission(), ack)) {
        setMessageSubmission(null);
        retryableMessageFrame = null;
      }
    },
  });
}

export function dismissMessageSubmission(): void {
  if (messageSubmission()?.phase === 'sending' || messageSubmission()?.phase === 'accepted') return;
  setMessageSubmission(null);
  retryableMessageFrame = null;
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
  setCancellingTurn(true);
  sendAction(H.cancel(currentCid), 'cancel', {
    cb: () => setCancellingTurn(false),
    onError: () => setCancellingTurn(false),
    onTimeout: () => { setCancellingTurn(false); persistActionProblem('停止结果尚未确认', '停止请求可能仍在执行。请根据流式状态判断，不要连续提交相反操作。'); },
  });
}

export function closeChat(onFinished?: () => void): boolean {
  if (!ready || readOnly()) {
    toast('连接未就绪，稍后再试');
    return false;
  }
  if (!currentCid || closingChat()) return false;
  if (isTerminal(chatStatusSignal()[currentCid])) {
    toast('对话已结束，无需关闭');
    return false;
  }
  setClosingChat(true);
  return sendAction(H.close(currentCid), '关闭运行实例', {
    cb: () => { setClosingChat(false); onFinished?.(); toast('运行实例已关闭，会话仍保存在项目中'); },
    onError: () => setClosingChat(false),
    onTimeout: () => {
      setClosingChat(false);
      onFinished?.();
      persistActionProblem('关闭结果尚未确认', '运行实例可能已经关闭。左侧持久会话不会被删除；请等待状态同步后再决定是否重新打开。');
    },
  });
}

export function resolvePermission(permissionId: string, decision: string): void {
  if (!ready || readOnly()) {
    if (readOnly()) return toast('只读模式不能处理权限请求');
    toast('连接未就绪，稍后再试');
    return;
  }
  if (!currentCid) return;
  if (pendingPermissionDecisions().has(permissionId)) return;
  setPendingPermissionDecisions((decisions) => lockPermissionDecision(decisions, permissionId, decision));
  const frame = H.resolvePermission(currentCid, permissionId, decision);
  permissionCommands.set(frame.commandId, permissionId);
  const release = () => {
    setPendingPermissionDecisions((decisions) => unlockPermissionDecision(decisions, permissionId));
    permissionCommands.delete(frame.commandId);
  };
  sendAction(frame, 'resolve', {
    cb: () => permissionCommands.delete(frame.commandId),
    onError: release,
    onTimeout: () => persistActionProblem('权限决策结果尚未确认', '请等待请求从界面消失或返回错误，不要提交相反决策。', frame.commandId),
  });
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
