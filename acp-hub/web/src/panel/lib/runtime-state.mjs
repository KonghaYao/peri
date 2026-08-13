const TERMINAL_LABELS = {
  ended: '运行已结束 · 会话已保留',
  closed: '运行已关闭 · 会话已保留',
  crashed: '运行异常退出 · 会话已保留',
};

/** User-facing state of one durable session and its optional runtime chat. */
export function runtimeState(input) {
  if (!input.hasSession) return { label: '', tone: 'idle' };
  if (input.lifecycle === 'reconciliation_required') return { label: '需要服务端对账', tone: 'attention' };
  if (input.lifecycle === 'failed') return { label: '打开失败', tone: 'danger' };
  if (input.isOpening || ['activating', 'pending'].includes(input.lifecycle)) return { label: '正在恢复 ACP 会话…', tone: 'busy' };
  if (!input.hasRuntime) return { label: '未启动 · 会话已保存', tone: 'idle' };

  const chatStatus = String(input.chatStatus || '').toLowerCase();
  if (TERMINAL_LABELS[chatStatus]) {
    return { label: TERMINAL_LABELS[chatStatus], tone: chatStatus === 'crashed' ? 'danger' : 'idle' };
  }
  if (input.hasPendingPermission) return { label: '等待你的许可', tone: 'attention' };
  if (input.isHydrated === false) return { label: '正在载入会话…', tone: 'busy' };
  if (input.turnActive) return { label: 'Agent 正在工作', tone: 'busy' };
  return { label: '可输入 · 会话已保存', tone: 'ready' };
}
