export const beginMessageSubmission = (commandId, text, sessionId = null, chatId = null) => ({
  commandId,
  text,
  sessionId,
  chatId,
  phase: 'sending',
  detail: null,
  retryable: true,
});

export const acceptMessageSubmission = (current, commandId) =>
  current?.commandId === commandId ? { ...current, phase: 'accepted', detail: null } : current;

export const markMessageUncertain = (current, commandId) =>
  current?.commandId === commandId
    ? { ...current, phase: 'uncertain', detail: '服务器尚未确认结果。重新确认不会重复执行。', retryable: true }
    : current;

export const failMessageSubmission = (current, commandId, detail, retryable = false) =>
  current?.commandId === commandId
    ? { ...current, phase: 'failed', detail, retryable }
    : current;

export const completesMessageSubmission = (current, ack) =>
  !!current && ack?.commandId === current.commandId && ['committed', 'duplicate'].includes(ack.status);

export const isTurnActive = (activeTurn) => {
  if (!activeTurn) return false;
  const status = String(activeTurn.turnStatus || '').toLowerCase();
  return !['completed', 'complete', 'interrupted', 'cancelled', 'canceled', 'failed', 'error', 'ended'].includes(status);
};

export const lockPermission = (ids, permissionId) =>
  ids.has(permissionId) ? ids : new Set([...ids, permissionId]);

export const unlockPermission = (ids, permissionId) => {
  if (!ids.has(permissionId)) return ids;
  const next = new Set(ids);
  next.delete(permissionId);
  return next;
};

export const lockPermissionDecision = (decisions, permissionId, decision) => {
  if (decisions.has(permissionId)) return decisions;
  const next = new Map(decisions);
  next.set(permissionId, { decision, phase: 'pending' });
  return next;
};

export const markPermissionDecisionUncertain = (decisions, permissionId) => {
  const current = decisions.get(permissionId);
  if (!current || current.phase === 'uncertain') return decisions;
  const next = new Map(decisions);
  next.set(permissionId, { ...current, phase: 'uncertain' });
  return next;
};

export const unlockPermissionDecision = (decisions, permissionId) => {
  if (!decisions.has(permissionId)) return decisions;
  const next = new Map(decisions);
  next.delete(permissionId);
  return next;
};

export const beginQuickStart = (commandId, projectId, text) => ({
  commandId,
  projectId,
  text,
  phase: 'creating',
  detail: null,
  retryable: true,
});

export const updateQuickStart = (current, commandId, phase, detail = null, retryable = current?.retryable ?? true) =>
  current?.commandId === commandId ? { ...current, phase, detail, retryable } : current;

export const quickStartCanActivate = (current, ack) =>
  !!current && ack?.commandId === current.commandId && ['committed', 'duplicate'].includes(ack.status)
    && typeof ack.sessionId === 'string' && !!ack.sessionId && typeof ack.chatId === 'string' && !!ack.chatId;
