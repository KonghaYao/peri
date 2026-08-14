// acp-hub Web 面板 —— 装配与编排（SolidJS 响应式 store）。
//
// 流程（M3 方案 §4，移植自原 main.js）：
//   1. HttpOnly Cookie 鉴权 → ysync.subscribe ["hub:registry"]
//      → 快照 + ready → UI 启用。
//   2. registry 渲染 → 左栏实例/对话。
//   3. 点击对话 → subscribe ["chat:{cid}","session:{cid}"] → 快照渲染历史
//      → 增量实时更新（yjs 流式）。
//   4. 发送消息 → chat/prompt（CommandTracker 跟踪 accepted→terminal）;
//      用户消息依赖 server 投影，本地只保留可恢复的提交状态。
//   5. create committed ack（带 chatId）→ 自动订阅 chat:{cid} 并选中。
//   6. 断线：4500/4501/4502 停止并提示；1011/1013 指数退避重连（ws-client），
//      重连后重放订阅（快照兜底）。

import { createSignal } from 'solid-js';
import type { Setter } from 'solid-js';
import * as H from './lib/protocol';
import { DocStore } from './lib/doc-store';
import { renderChat, type ChatEntry } from './lib/chat-view';
import { renderControl, type ControlView } from './lib/control-view';
import { renderRegistry, type ChatInfo, type ProjectInfo, type ProjectSessionInfo, type SessionSummaryInfo } from './lib/registry-view';
import { WsClient } from './lib/ws-client';
import type { ConnStatus, ConnDetail, WsProtocolIssue } from './lib/ws-client';
import { unimportedSessions } from './lib/session-import.mjs';
import { isTurnActive } from './lib/action-state.mjs';
import { retainLiveRuntimeHints } from './lib/recovery-state.mjs';
import { connectionTransition } from './lib/connection-state.mjs';
import { CommandTracker } from './lib/command-tracker';
import { SessionActivation, type OpeningSession, type OpenSessionCallbacks } from './lib/session-activation';
import { installPrincipalRole, principalRole, publishAuthInvalidation, readOnly } from './lib/auth-state';
import { acceptMessageDelivery, blockUnknownMessageDelivery, completeMessageDelivery, failMessageDelivery, markMessageDeliveryUncertain, messageSubmission, reconcileMessageProjection, resetMessageDelivery, retryMessageDelivery, startMessageDelivery } from './lib/message-delivery';
import { settleLateQuickStart } from './lib/quick-start-delivery';
import { acceptRuntimeControl, confirmRuntimeControl, failRuntimeControl, markRuntimeControlUncertain, reconcileRuntimeControl, resetRuntimeControls, retryRuntimeControl, runtimeControlBusy, startRuntimeControl } from './lib/runtime-control';
import { failPermissionDecision, markPermissionDecisionUncertain, resetPermissionDecisions, retainProjectedPermissions, retryPermissionDecision, startPermissionDecision, type PermissionDecision } from './lib/permission-delivery';
import { CatalogActions } from './lib/catalog-actions';
import { ToastStore } from './lib/toast-store';
import type { PromptStatusItem } from './lib/protocol';

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
export const [promptDeliveryReady, setPromptDeliveryReady] = createSignal(false);
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
export const [openingSession, setOpeningSession] = createSignal<OpeningSession | null>(null);
export const openingSessionId = () => openingSession()?.sessionId ?? null;
export interface RuntimeDocsState { chat: boolean; control: boolean }
export const [runtimeDocsState, setRuntimeDocsState] = createSignal<RuntimeDocsState>({ chat: false, control: false });
export const runtimeDocsHydrated = () => runtimeDocsState().chat && runtimeDocsState().control;
/** 当前选中的工作区 id（null = 全部）：左栏对话/会话按此过滤。 */
export const [chatStatusSignal, setChatStatusSignal] = createSignal<Record<string, string>>({});
const toastStore = new ToastStore();
export const toasts = toastStore.records;
export interface PersistentError { id: number; title: string; detail: string; commandId: string | null; retryable: boolean; retrying: boolean }
export const [persistentErrors, setPersistentErrors] = createSignal<PersistentError[]>([]);
export const turnActive = () => isTurnActive(chatHead()?.activeTurn);
export interface ConnectionProblem { code: number | null; title: string; detail: string; action: 'reconnect' | 'login' | null }
export const [connectionProblem, setConnectionProblem] = createSignal<ConnectionProblem | null>(null);
export const [restoringSessionId, setRestoringSessionId] = createSignal<string | null>(null);
export const [creatingSessionProjectId, setCreatingSessionProjectId] = createSignal<string | null>(null);
export const [discoveringSessionsProjectId, setDiscoveringSessionsProjectId] = createSignal<string | null>(null);
export interface PromptRecoveryView {
  sessionId: string;
  loading: boolean;
  truncated: boolean;
  evidenceIncomplete: boolean;
  error: string | null;
  prompts: PromptStatusItem[];
}
export const [promptRecovery, setPromptRecovery] = createSignal<PromptRecoveryView | null>(null);

// ── 内部状态 ────────────────────────────────────────────────────────────

const store = new DocStore(); // docId → Y.Doc
let ws: WsClient | null = null; // 当前 WsClient
let connectionEpoch = 0; // 隔离被替换连接的延迟 status/frame 回调
let currentCid: string | null = null; // 选中对话（重连后恢复订阅）
let ready = false; // ready 门控：就绪后才发 action
type ActionFrame = ReturnType<typeof H.action>;
interface ActionOptions {
  cb?: (ack: Ack) => void;
  onAccepted?: (ack: Ack) => void;
  onTimeout?: () => void;
  onError?: (err: ActionError) => void;
  retryOnUncertain?: boolean;
  retryOnError?: boolean;
}
const [uncertainMetadataCount, setUncertainMetadataCount] = createSignal(0);
const LAST_SESSION_KEY = 'acp-hub:last-session';
let registryReceived = false;

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

const commands = new CommandTracker<ActionFrame, Ack, ActionError>({
  timeoutMs: ACK_TIMEOUT_MS,
  onUncertainCountChange: setUncertainMetadataCount,
  onFallbackUncertain: (request, reason) => persistActionProblem(
    '结果尚未确认',
    reason === 'disconnect'
      ? `${request.label} 在连接中断前未收到终态回复。服务器可能仍会完成操作，请先等待状态同步，不要盲目重复执行。`
      : `${request.label} 已超过 30 秒未收到终态回复。服务器可能仍会完成操作，请先等待状态同步，不要盲目重复执行。`,
    request.frame.commandId,
  ),
});

let problemSeq = 0;

// ── toast ───────────────────────────────────────────────────────────────

export function toast(msg: string): void {
  toastStore.show(msg);
}

export function dismissPersistentError(id: number): void {
  setPersistentErrors((items) => {
    const target = items.find((item) => item.id === id);
    if (target?.commandId) commands.forget(target.commandId);
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
    id: ++problemSeq, title, detail, commandId: commandId || null, retryable: !!commandId && commands.hasUncertain(commandId), retrying: false,
  }));
}

export function reportTransportIssue(issue: WsProtocolIssue): void {
  const problem = issue.kind === 'non_text_frame'
    ? { title: '收到不支持的服务器数据', detail: `WebSocket 下行帧不是文本（${issue.size} 字节），已安全忽略且未记录其内容。` }
    : issue.kind === 'malformed_frame'
      ? { title: '收到格式错误的服务器数据', detail: `WebSocket 下行文本无法识别（${issue.size} 个字符），已安全忽略且未记录其内容。` }
      : issue.kind === 'send_error'
        ? { title: '数据未能写入连接', detail: '浏览器连接在发送瞬间失效。相关操作没有被登记为已发送；消息草稿和可恢复状态仍会保留。' }
        : { title: '页面未能处理服务器更新', detail: `浏览器的 ${issue.callback === 'frame' ? '下行数据' : '连接状态'} 处理发生异常。连接仍保持，后续更新会继续处理。` };
  setPersistentErrors((items) => {
    const withoutDuplicate = items.filter((item) => item.title !== problem.title);
    return retainPersistentErrors(withoutDuplicate, {
      id: ++problemSeq,
      ...problem,
      commandId: null,
      retryable: false,
      retrying: false,
    });
  });
}

export function retryPersistentAction(commandId: string): boolean {
  if (!commands.hasUncertain(commandId) || !ready) {
    toast(!commands.hasUncertain(commandId) ? '此操作不能安全重试' : '连接未就绪，暂时不能重新确认');
    return false;
  }
  if (commands.hasPending(commandId)) return false;
  const sent = commands.retry(commandId, sendFrame) === 'sent';
  if (sent) setPersistentErrors((items) => items.map((item) => item.commandId === commandId
    ? { ...item, title: '正在重新确认', detail: '正在使用原 commandId 查询或完成此操作，不会创建第二个请求。', retryable: false, retrying: true }
    : item));
  if (sent) retryRuntimeControl(commandId);
  if (sent) retryPermissionDecision(commandId);
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

function reconcileCurrentRuntimeControl(control: ControlView | null = chatHead()): void {
  if (!currentCid) return;
  reconcileRuntimeControl(
    currentCid,
    isTurnActive(control?.activeTurn),
    isTerminal(chatStatusSignal()[currentCid]),
  );
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
  setRuntimeDocsState({ chat: false, control: false });
  setSubscribedDocs('—'); // 订阅清单等下一帧 ready 刷新（简单置空亦可）
}

// ── ack 表 ──────────────────────────────────────────────────────────────

// 发送 action 并登记 ack 回调；ready 前不发（server 会缓冲，但面板
// 以 ready 门控保证可预期）。
function sendFrame(frame: ActionFrame): boolean { return !!ws?.send(frame); }

function sendAction(frame: ActionFrame, label: string, options: ActionOptions = {}): boolean {
  const result = commands.dispatch({
    frame,
    label,
    callbacks: {
      onAccepted: options.onAccepted,
      onTerminal: options.cb,
      onError: options.onError,
      retryOnUncertain: options.retryOnUncertain,
      retryOnError: options.retryOnError,
      onUncertain: options.onTimeout,
    },
  }, sendFrame);
  if (result !== 'sent') {
    if (result === 'already_pending') return false;
    toast(`连接未就绪，无法发送 ${label}`);
    options.onError?.({ commandId: frame.commandId, code: 'UNAVAILABLE', message: '连接未就绪', retryable: true });
    return false;
  }
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
    case 'prompt_status': {
      const response = frame as { commandId: string; sessionId: string; truncated: boolean; evidenceIncomplete: boolean; prompts: PromptStatusItem[] };
      if (response.sessionId === selectedSessionId()) {
        setPromptRecovery({
          sessionId: response.sessionId,
          loading: false,
          truncated: response.truncated,
          evidenceIncomplete: response.evidenceIncomplete,
          error: null,
          prompts: response.prompts,
        });
      }
      commands.acknowledge({ commandId: response.commandId, status: 'committed' });
      break;
    }
    case 'auth_error':
      invalidateAuthentication('访问令牌已失效、被撤销或服务器已重启，需要重新登录。');
      break;
    default:
      break; // 未知帧忽略（协议演进兼容）
  }
}

function onAck(ack: Ack): void {
  showAck(ack);
  const disposition = commands.acknowledge(ack);
  // A terminal acknowledgement that arrives after timeout/disconnect can
  // reconcile local uncertainty, but must not replay an expired continuation.
  if (disposition === 'late_terminal') {
    const completedSubmission = messageSubmission();
    if (completedSubmission && ack.commandId) completeMessageDelivery(ack.commandId, ack.status);
    if (ack.commandId) settleLateQuickStart(ack.commandId, ack.status, ack.sessionId, ack.chatId);
    if (ack.commandId && confirmRuntimeControl(ack.commandId, ack.status)) reconcileCurrentRuntimeControl();
  }
  if (ack.status !== 'accepted' && ack.commandId) {
    setPersistentErrors((items) => items.filter((item) => item.commandId !== ack.commandId));
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
  DELIVERY_UNKNOWN: '投递结果未知',
};

function onActionError(err: ActionError): void {
  console.error(`[panel] action 错误 code=${err.code || 'UNKNOWN'} command=${err.commandId ? 'present' : 'absent'}`);
  showActionError(err);
  commands.fail(err);
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
      if (isTerminal(s.status || undefined)) reconcileRuntimeControl(s.id, true, true);
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
    reconcileSessionNavigation(projectedSessions);
    return;
  }
  if (currentCid && docId === H.chatDoc(currentCid)) {
    const conv = renderChat(store.docFor(docId));
    setChatEntries(conv.entries);
    reconcileMessageProjection(new Set(conv.entries
      .map((entry) => entry.sourceCommandId)
      .filter((commandId): commandId is string => commandId !== null)));
    setRuntimeDocsState((state) => ({ ...state, chat: true }));
    return;
  }
  if (currentCid && docId === H.sessionDoc(currentCid)) {
    const ctrl = renderControl(store.docFor(docId));
    setChatHead(ctrl);
    setPermissions(ctrl.pendingPermissions);
    setRuntimeDocsState((state) => ({ ...state, control: true }));
    const visiblePermissionIds = new Set(ctrl.pendingPermissions.map((item) => item.permissionId).filter((id): id is string => !!id));
    retainProjectedPermissions(visiblePermissionIds);
    reconcileCurrentRuntimeControl(ctrl);
  }
};

// ── 连接状态机回调（ws-client）─────────────────────────────────────────

function onStatus(state: ConnStatus, detail: ConnDetail): void {
  const transition = connectionTransition(state, detail, !!principalRole());
  if (transition) {
    ready = transition.ready;
    setBusy(transition.busy);
    setConnState(transition.status);
    setConnectionProblem(transition.problem);
  }
  switch (state) {
    case 'connecting':
      setPromptDeliveryReady(false);
      break;
    case 'open':
      // 已发 auth；认证后首帧必须是 ysync.subscribe 或 action ——
      // 立即重放订阅（首次连接与重连同一路径，快照兜底）。
      sendSubscribe();
      break;
    case 'ready':
      setPromptDeliveryReady(
        Array.isArray(detail.negotiatedCapabilities)
          && detail.negotiatedCapabilities.includes(H.CAP_PROMPT_DELIVERY_V2),
      );
      if (registryReceived) reconcileSessionNavigation(projectSessions());
      if (selectedSessionId()) requestPromptRecovery(selectedSessionId()!);
      setSubscribedDocs(
        detail.projectionVersions
          ? Object.keys(detail.projectionVersions as Record<string, unknown>).join('、')
          : '—',
      );
      break;
    case 'heartbeat':
      setHeartbeatCount((c) => c + 1);
      break;
    case 'reconnecting':
      commands.settleConnectionLoss();
      break;
    case 'fatal':
      commands.settleConnectionLoss();
      sessionActivation.connectionLost();
      if (detail.code === 4502) {
        invalidateAuthentication('浏览器会话已失效、访问令牌被撤销，或服务器认证配置发生变化。请重新登录。');
        break;
      }
      toast(
        `连接终止（${detail.code}）：` +
        (detail.code === 4500 ? '实例离线' :
          detail.code === 4501 ? '心跳超时' :
          detail.code === 4502 ? '认证失败/配置性失败' : '未知原因') +
        '，不自动重连',
      );
      break;
    case 'closed':
      commands.settleConnectionLoss();
      sessionActivation.connectionLost();
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
  commands.settleConnectionLoss();
  const epoch = ++connectionEpoch;
  if (ws) ws.close();
  const client = new WsClient({
    url: wsUrl(),
    onStatus: (state, detail) => { if (epoch === connectionEpoch) onStatus(state, detail); },
    onFrame: (frame) => { if (epoch === connectionEpoch) onFrame(frame); },
    onProtocolIssue: (issue) => { if (epoch === connectionEpoch) reportTransportIssue(issue); },
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

function readRememberedSession(): string | null {
  let preferred: string | null = null;
  try { preferred = window.localStorage.getItem(LAST_SESSION_KEY); } catch { /* 手动选择兜底 */ }
  return preferred;
}

function forgetRememberedSession(): void {
  try { window.localStorage.removeItem(LAST_SESSION_KEY); } catch { /* UI preference only */ }
}

function clearCurrentSelection(): void {
  setSelectedSessionId(null);
  const previousCid = currentCid;
  if (previousCid && ws) ws.send(H.unsubscribe([H.chatDoc(previousCid), H.sessionDoc(previousCid)]));
  currentCid = null;
  setSelectedCid(null);
  setChatEntries([]);
  setChatHead(null);
  setPermissions([]);
  setRuntimeDocsState({ chat: false, control: false });
  setPromptRecovery(null);
  forgetRememberedSession();
}

function activateSession(sessionId: string, chatId: string): void {
  setSelectedSessionId(sessionId);
  rememberSession(sessionId);
  selectChat(chatId);
  requestPromptRecovery(sessionId);
}

function requestPromptRecovery(sessionId: string): void {
  if (!ready) return;
  if (promptRecovery()?.sessionId === sessionId && promptRecovery()?.loading) return;
  setPromptRecovery({ sessionId, loading: true, truncated: false, evidenceIncomplete: false, error: null, prompts: [] });
  const frame = H.persistedSessionPromptStatus(sessionId);
  sendAction(frame, '读取消息恢复状态', {
    cb: () => undefined,
    retryOnUncertain: true,
    onTimeout: () => {
      if (selectedSessionId() === sessionId) {
        setPromptRecovery({ sessionId, loading: false, truncated: false, evidenceIncomplete: true, error: '恢复状态查询超时；这是只读操作，可以安全重试。', prompts: [] });
      }
    },
    onError: (failure) => {
      if (selectedSessionId() === sessionId) {
        setPromptRecovery({ sessionId, loading: false, truncated: false, evidenceIncomplete: true, error: failure.message || '暂时无法读取历史恢复状态。', prompts: [] });
      }
    },
  });
}

const sessionActivation = new SessionActivation({
  isReady: () => ready,
  isReadOnly: readOnly,
  hasUncertainMetadata: () => !!uncertainMetadataCount(),
  hasMessageSubmission: () => !!messageSubmission(),
  creatingProjectId: creatingSessionProjectId,
  setCreatingProjectId: setCreatingSessionProjectId,
  sessions: projectSessions,
  selectedSessionId,
  currentChatId: () => currentCid,
  preferredSessionId: readRememberedSession,
  send: sendAction,
  retry: (commandId) => commands.retry(commandId, sendFrame) ?? 'missing',
  hasUncertainCommand: (commandId) => commands.hasUncertain(commandId),
  activate: activateSession,
  forgetPreference: forgetRememberedSession,
  sendFirstMessage: (text) => sendMessage(text),
  onNavigationChange: (snapshot) => {
    setOpeningSession(snapshot.opening);
    setRestoringSessionId(snapshot.restoringSessionId);
  },
  toast,
  persistProblem: persistActionProblem,
});

function reconcileSessionNavigation(sessions: ProjectSessionInfo[]): void {
  sessionActivation.reconcileCatalog(sessions);
}

const catalogActions = new CatalogActions({
  isReady: () => ready,
  isReadOnly: readOnly,
  hasUncertainMetadata: () => !!uncertainMetadataCount(),
  send: sendAction,
  toast,
  persistProblem: persistActionProblem,
  onProjectArchived: (projectId) => {
    const selected = projectSessions().find((session) => session.id === selectedSessionId());
    if (selected?.projectId === projectId) clearCurrentSelection();
  },
  onSessionArchived: (sessionId) => {
    if (selectedSessionId() === sessionId) clearCurrentSelection();
  },
  discoveringProjectId: discoveringSessionsProjectId,
  setDiscoveringProjectId: setDiscoveringSessionsProjectId,
});

export const createProject = (name: string, cwd: string, onCommitted?: () => void, onFailed?: () => void) =>
  catalogActions.createProject(name, cwd, { onCommitted, onFailed });
export const archiveProject = (projectId: string, onCommitted?: () => void, onFailed?: () => void) =>
  catalogActions.archiveProject(projectId, { onCommitted, onFailed });
export const restoreProject = (projectId: string, onCommitted?: () => void, onFailed?: () => void) =>
  catalogActions.restoreProject(projectId, { onCommitted, onFailed });
export const renameProject = (projectId: string, name: string, onCommitted?: () => void, onFailed?: () => void) =>
  catalogActions.renameProject(projectId, name, { onCommitted, onFailed });

export const createProjectSession = (projectId: string, title?: string): boolean => sessionActivation.create(projectId, title);
export const createSessionWithFirstMessage = (projectId: string, text: string): boolean => sessionActivation.quickStart(projectId, text);
export const retryQuickStart = (): void => sessionActivation.retryQuickStart();

export const renameProjectSession = (sessionId: string, name: string, onCommitted?: () => void, onFailed?: () => void) =>
  catalogActions.renameSession(sessionId, name, { onCommitted, onFailed });
export const archiveProjectSession = (sessionId: string, onCommitted?: () => void, onFailed?: () => void) =>
  catalogActions.setSessionArchived(sessionId, true, { onCommitted, onFailed });
export const restoreProjectSession = (sessionId: string, onCommitted?: () => void, onFailed?: () => void) =>
  catalogActions.setSessionArchived(sessionId, false, { onCommitted, onFailed });
export const importProjectSession = (projectId: string, acpSessionId: string, onCommitted?: () => void, onFailed?: (kind: 'failed' | 'uncertain') => void) =>
  catalogActions.importSession(projectId, acpSessionId, onCommitted, onFailed);
export const discoverProjectSessions = (projectId: string, onCommitted?: () => void, onFailed?: (message: string) => void) =>
  catalogActions.discoverSessions(projectId, onCommitted, onFailed);

export function disconnect(): void {
  commands.settleConnectionLoss();
  connectionEpoch += 1;
  ready = false;
  setPromptDeliveryReady(false);
  sessionActivation.connectionLost();
  if (ws) {
    ws.close();
    ws = null;
  }
  setBusy(false);
}

export function resetAuthenticatedSession(): void {
  // Revoke mutation authority before settling callbacks from the old transport.
  // This function is intentionally idempotent: both the invalidation producer
  // and AuthGate consumer call it to make the identity boundary fail closed.
  installPrincipalRole(null);
  disconnect();
  currentCid = null;
  setConnState({ text: '未连接', kind: 'idle' });
  setHeartbeatCount(0);
  setGlobalStatus('');
  setSubscribedDocs('—');
  setPromptDeliveryReady(false);
  setAckLog([]);
  setErrorLog([]);
  setChats([]);
  setSelectedCid(null);
  setSelectedSessionId(null);
  setChatEntries([]);
  setChatHead(null);
  setPermissions([]);
  setRuntimeDocsState({ chat: false, control: false });
  setChatStatusSignal({});
  setProjects([]);
  setProjectSessions([]);
  setImportableSessions([]);
  setPromptRecovery(null);
  resetMessageDelivery();
  sessionActivation.reset();
  resetRuntimeControls();
  setDiscoveringSessionsProjectId(null);
  setPersistentErrors([]);
  commands.reset();
  resetPermissionDecisions();
  setConnectionProblem(null);
  registryReceived = false;
  store.clear();
  // Keep this last: disconnect/reset callbacks are allowed to publish feedback,
  // but no notification from the previous principal may survive this boundary.
  toastStore.clear();
}

export function navigateProjectSession(sessionId: string, callbacks: OpenSessionCallbacks = {}): boolean {
  return sessionActivation.navigate(sessionId, callbacks);
}

// ── 用户动作 → action ──────────────────────────────────────────────────

export function sendMessage(text: string, effort?: string): boolean {
  if (!ready || !promptDeliveryReady() || readOnly() || openingSessionId() || turnActive() || messageSubmission()) {
    if (!promptDeliveryReady()) { toast('服务器尚未启用安全消息投递，请刷新或升级 server'); return false; }
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
  const sessionId = selectedSessionId();
  if (!sessionId) {
    toast('持久会话尚未就绪');
    return false;
  }
  if (isTerminal(chatStatusSignal()[currentCid])) {
    toast('对话已结束，不能发送消息');
    return false;
  }
  const frame = H.prompt(currentCid, text, effort);
  if (!startMessageDelivery(frame.commandId, text, sessionId, currentCid)) {
    toast('上一条消息仍在确认中');
    return false;
  }
  const sent = sendAction(frame, 'prompt', {
    retryOnUncertain: true,
    onAccepted: () => acceptMessageDelivery(frame.commandId),
    onTimeout: () => markMessageDeliveryUncertain(frame.commandId),
    onError: (err) => err.code === 'DELIVERY_UNKNOWN'
      ? blockUnknownMessageDelivery(frame.commandId, err.message)
      : failMessageDelivery(frame.commandId, err.message || '消息提交失败'),
    cb: (ack) => completeMessageDelivery(frame.commandId, ack.status),
  });
  return sent;
}

export function retryMessageSubmission(): void {
  const current = messageSubmission();
  if (!current || current.phase !== 'uncertain' || !commands.hasUncertain(current.commandId)) return;
  if (!ready) return toast('连接未就绪，暂时不能重新确认');
  const result = commands.retry(current.commandId, sendFrame);
  if (result === 'sent') retryMessageDelivery(current.commandId);
  else toast('连接未就绪，原请求仍已保留');
}

function invalidateAuthentication(reason: string): void {
  resetAuthenticatedSession();
  publishAuthInvalidation(reason);
}

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
  const chatId = currentCid;
  const frame = H.cancel(chatId);
  if (!startRuntimeControl(frame.commandId, chatId, 'cancel')) return toast('此运行实例已有控制请求正在确认');
  sendAction(frame, 'cancel', {
    retryOnUncertain: true,
    onAccepted: () => acceptRuntimeControl(frame.commandId),
    cb: (ack) => { if (confirmRuntimeControl(frame.commandId, ack.status)) reconcileCurrentRuntimeControl(); },
    onError: (error) => failRuntimeControl(frame.commandId, error.message || '无法停止当前生成。'),
    onTimeout: () => {
      markRuntimeControlUncertain(frame.commandId);
      persistActionProblem('停止结果尚未确认', '停止请求可能仍在执行。请使用原请求重新确认，不要连续提交新的停止操作。', frame.commandId);
    },
  });
}

export function closeChat(onFinished?: () => void): boolean {
  if (!ready || readOnly()) {
    toast('连接未就绪，稍后再试');
    return false;
  }
  if (!currentCid || runtimeControlBusy(currentCid)) return false;
  if (isTerminal(chatStatusSignal()[currentCid])) {
    toast('对话已结束，无需关闭');
    return false;
  }
  const chatId = currentCid;
  const frame = H.close(chatId);
  if (!startRuntimeControl(frame.commandId, chatId, 'close')) return false;
  return sendAction(frame, '关闭运行实例', {
    retryOnUncertain: true,
    onAccepted: () => acceptRuntimeControl(frame.commandId),
    cb: (ack) => {
      if (!confirmRuntimeControl(frame.commandId, ack.status)) return;
      reconcileCurrentRuntimeControl();
      onFinished?.();
      toast('运行实例已关闭，会话仍保存在项目中');
    },
    onError: (error) => failRuntimeControl(frame.commandId, error.message || '无法关闭运行实例。'),
    onTimeout: () => {
      markRuntimeControlUncertain(frame.commandId);
      onFinished?.();
      persistActionProblem('关闭结果尚未确认', '运行实例可能已经关闭。左侧持久会话不会被删除；请使用原请求重新确认。', frame.commandId);
    },
  });
}

export function resolvePermission(permissionId: string, decision: PermissionDecision): void {
  if (!ready || readOnly()) {
    if (readOnly()) return toast('只读模式不能处理权限请求');
    toast('连接未就绪，稍后再试');
    return;
  }
  if (!currentCid) return;
  const frame = H.resolvePermission(currentCid, permissionId, decision);
  if (!startPermissionDecision(frame.commandId, permissionId, decision)) return;
  sendAction(frame, 'resolve', {
    retryOnError: true,
    onError: (error) => error.retryable
      ? markPermissionDecisionUncertain(frame.commandId, true)
      : failPermissionDecision(frame.commandId),
    onTimeout: () => {
      markPermissionDecisionUncertain(frame.commandId);
      persistActionProblem('权限决策结果尚未确认', '请等待请求从界面消失或返回错误，不要提交相反决策。', frame.commandId);
    },
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
