import { connectionProblemForClose } from './recovery-state.mjs';

/**
 * One authoritative presentation/gating transition for a WebSocket lifecycle
 * event. Domain effects (settling commands, resubscribing, clearing auth) stay
 * in the store; callers cannot independently reinterpret readiness and UI.
 */
export function connectionTransition(state, detail = {}, hasPrincipal = false) {
  switch (state) {
    case 'connecting':
      return { ready: false, busy: true, status: { text: '连接中…', kind: 'idle' }, problem: null };
    case 'open':
      return { ready: false, busy: true, status: { text: '已认证', kind: 'ok' }, problem: null };
    case 'ready':
      return { ready: true, busy: false, status: { text: '就绪', kind: 'ok' }, problem: null };
    case 'reconnecting':
      return {
        ready: false,
        busy: true,
        status: { text: `重连中（${Math.round((detail.retryMs || 0) / 1000)}s 后）`, kind: 'warn' },
        problem: null,
      };
    case 'fatal':
      return {
        ready: false,
        busy: false,
        status: { text: `已停止（${detail.code ?? '未知'}）`, kind: 'err' },
        problem: connectionProblemForClose(detail.code),
      };
    case 'closed':
      return {
        ready: false,
        busy: false,
        status: { text: '已断开', kind: 'idle' },
        problem: hasPrincipal
          ? { code: null, title: '连接已断开', detail: '当前页面没有连接到 acp-hub server。你的持久会话仍然安全。', action: 'reconnect' }
          : null,
      };
    case 'heartbeat':
      return null;
    default:
      return null;
  }
}
