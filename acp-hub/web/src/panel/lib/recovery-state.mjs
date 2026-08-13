export const chooseRestorableSession = (preferredId, sessions) => {
  if (!preferredId) return null;
  const session = sessions.find((item) => item.id === preferredId);
  return session?.lifecycle === 'ready' ? session : null;
};

const TERMINAL_RUNTIME = new Set(['ended', 'closed', 'crashed']);

export const retainLiveRuntimeHints = (sessions, chats) => {
  const live = new Set(chats.filter((chat) => !TERMINAL_RUNTIME.has(String(chat.status || ''))).map((chat) => chat.id));
  return sessions.map((session) => ({
    ...session,
    activeChatId: session.activeChatId && live.has(session.activeChatId) ? session.activeChatId : null,
  }));
};

export const cleanSessionTitle = (title) => {
  const raw = String(title || '')
    .replace(/<system-reminder>[\s\S]*$/i, '')
    .replace(/<\/??system-reminder[^>]*>/gi, '')
    .replace(/\s+/g, ' ')
    .trim();
  if (!raw) return '未命名会话';
  return raw.length > 72 ? `${raw.slice(0, 69).trimEnd()}…` : raw;
};

export const formatRelativeTime = (value, now = Date.now()) => {
  const timestamp = Date.parse(value || '');
  if (!Number.isFinite(timestamp)) return '时间未知';
  const seconds = Math.max(0, Math.round((now - timestamp) / 1000));
  if (seconds < 60) return '刚刚';
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days} 天前`;
  return new Intl.DateTimeFormat('zh-CN', { month: 'short', day: 'numeric', year: timestamp < new Date(now).setFullYear(new Date(now).getFullYear() - 1) ? 'numeric' : undefined }).format(timestamp);
};

export const shortSessionId = (sessionId) => {
  const value = String(sessionId || '');
  return value ? value.slice(-8) : '未知 ID';
};

export const sessionDisplayTitle = (title, stableId) => {
  const cleaned = cleanSessionTitle(title);
  if (!['新对话', '未命名会话'].includes(cleaned)) return cleaned;
  return `${cleaned} · …${shortSessionId(stableId)}`;
};

export const connectionProblemForClose = (code) => {
  if (code === 4500) return {
    code, title: '本地 Agent 实例已离线',
    detail: '服务器仍可访问，但当前没有可用的 ACP 实例。确认 instance 进程运行后重新连接。',
    action: 'reconnect',
  };
  if (code === 4501) return {
    code, title: '与服务器的连接已超时',
    detail: '网络或页面休眠导致心跳中断。你的会话仍保存在服务器中。',
    action: 'reconnect',
  };
  if (code === 4502) return {
    code, title: '登录状态已失效',
    detail: '访问令牌已失效、被撤销或服务器已重启，需要重新登录。',
    action: 'login',
  };
  return {
    code: Number.isFinite(code) ? code : null,
    title: '连接已停止',
    detail: '连接意外终止，你的持久会话不会因此丢失。',
    action: 'reconnect',
  };
};
