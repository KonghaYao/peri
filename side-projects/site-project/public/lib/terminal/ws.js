// ========== Terminal WebSocket 管理器 ==========
// 封装：连接、重连（指数退避，最多 5 次）、session 持久化、消息分类（session/stats/data）。

import { buildWsUrl } from '/lib/env.js';

const SESSION_KEY = 'terminal-session';
const MAX_ATTEMPTS = 5;

/**
 * 创建一个 ws 连接。返回 { ws, send, close }。
 * @param {{ onOpen?: () => void, onMessage?: (data: string) => void, onClose?: () => void, onError?: () => void, getCols: () => number, getRows: () => number }} handlers
 */
export function connectTerminal(handlers) {
  const sid = readSession();
  const url = buildWsUrl(sid);
  const ws = new WebSocket(url);

  ws.onopen = () => {
    handlers.onOpen?.();
    // 通知服务端当前尺寸
    const cols = handlers.getCols?.() ?? 80;
    const rows = handlers.getRows?.() ?? 24;
    ws.send(JSON.stringify({ type: 'resize', cols, rows }));
  };

  ws.onmessage = (e) => {
    const parsed = parseMessage(e.data);
    if (parsed.kind === 'session') {
      writeSession(parsed.payload);
      return;
    }
    if (parsed.kind === 'stats') return;   // 暂不显示
    handlers.onMessage?.(parsed.payload);
  };

  ws.onclose = () => handlers.onClose?.();
  ws.onerror = () => { try { ws.close(); } catch {} handlers.onError?.(); };

  return {
    ws,
    send: (data) => { if (ws.readyState === WebSocket.OPEN) ws.send(data); },
    close: () => { try { ws.close(); } catch {} },
  };
}

/**
 * 指数退避重连调度。
 * @param {number} attempt 当前尝试次数（首次传 1）
 * @returns {{ delay: number, next: () => void, stop: boolean }}
 */
export function nextReconnectDelay(attempt) {
  if (attempt > MAX_ATTEMPTS) return { delay: 0, next: null, stop: true };
  const delay = Math.min(30000, 1000 * Math.pow(2, attempt - 1));
  return { delay, next: null, stop: false };
}

// 私有
function readSession() {
  try { return sessionStorage.getItem(SESSION_KEY); } catch { return null; }
}
function writeSession(sid) {
  try { if (sid) sessionStorage.setItem(SESSION_KEY, sid); } catch {}
}
function parseMessage(raw) {
  if (typeof raw !== 'string') return { kind: 'data', payload: raw };
  if (raw.charCodeAt(0) !== 123) return { kind: 'data', payload: raw };
  try {
    const m = JSON.parse(raw);
    if (m && typeof m === 'object' && typeof m.type === 'string') {
      if (m.type === 'session' && m.id) return { kind: 'session', payload: m.id };
      if (m.type === 'stats') return { kind: 'stats', payload: m };
    }
  } catch {}
  return { kind: 'data', payload: raw };
}

export { MAX_ATTEMPTS };
