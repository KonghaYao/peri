// acp-hub Web 面板 —— 装配与编排（SolidJS 响应式 store）。
//
// 流程（M3 方案 §4，移植自原 main.js）：
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

import { createSignal } from 'solid-js';
import type { Setter } from 'solid-js';
import * as H from './lib/protocol';
import { DocStore, renderChat, renderControl, renderRegistry } from './lib/yjs';
import type { ChatEntry, ChatInfo, ControlView, InstanceInfo } from './lib/yjs';
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
export const [chatStatusSignal, setChatStatusSignal] = createSignal<Record<string, string>>({});
export const [toasts, setToasts] = createSignal<{ id: number; msg: string }[]>([]);

// ── 内部状态 ────────────────────────────────────────────────────────────

const store = new DocStore(); // docId → Y.Doc
let ws: WsClient | null = null; // 当前 WsClient
let currentCid: string | null = null; // 选中对话（重连后恢复订阅）
let ready = false; // ready 门控：就绪后才发 action
const pendingAcks = new Map<string, { label: string; cb?: (ack: Ack) => void; timer: ReturnType<typeof setTimeout> }>();

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
    docs.push(H.controlDoc(currentCid));
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
  currentCid = cid;
  setSelectedCid(cid);
  sendSubscribe(); // 幂等；快照到达后由 onUpdate 渲染
  // 对话切换：清空旧渲染，等快照到达后重新填充。
  setChatEntries([]);
  setChatHead(null);
  setPermissions([]);
  setSubscribedDocs('—'); // 订阅清单等下一帧 ready 刷新（简单置空亦可）
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
  // 路径：自动补订阅 chat:{cid}/control:{cid} 并选中。
  if (ack.status === 'committed' && ack.chatId && ack.chatId !== currentCid) {
    toast(`对话已创建: ${ack.chatId.slice(0, 8)}…`);
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
    return;
  }
  if (currentCid && docId === H.chatDoc(currentCid)) {
    const conv = renderChat(store.docFor(docId));
    setChatEntries(conv.entries);
    return;
  }
  if (currentCid && docId === H.controlDoc(currentCid)) {
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
  if (ws) {
    ws.close();
    ws = null;
  }
  pendingAcks.forEach((p) => clearTimeout(p.timer));
  pendingAcks.clear();
  setBusy(false);
}

// ── 用户动作 → action ──────────────────────────────────────────────────

export function sendMessage(text: string): void {
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
  // 本地不造假，保证多标签页一致性）。
  sendAction(H.prompt(currentCid, text), 'prompt', (ack) => {
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
  // instanceId/cwd 留空 = 本机（payload 三字段全可选）。
  sendAction(H.createChat(), 'create', (ack) => {
    // chatId 已在 onAck 里统一处理（自动订阅选中）
    if (!ack.chatId) toast('create committed 缺少 chatId');
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
