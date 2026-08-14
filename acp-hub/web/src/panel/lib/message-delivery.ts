import { createSignal } from 'solid-js';

export type MessageDeliveryPhase = 'sending' | 'accepted' | 'committed' | 'uncertain' | 'delivery_unknown' | 'failed';

export interface MessageSubmission {
  commandId: string;
  text: string;
  sessionId: string;
  chatId: string;
  phase: MessageDeliveryPhase;
  detail: string | null;
  retryable: boolean;
  /** Exact durable entry has arrived; terminal command outcome is still pending. */
  projected: boolean;
}

const [currentSubmission, setCurrentSubmission] = createSignal<MessageSubmission | null>(null);
const [drafts, setDrafts] = createSignal<Record<string, string>>({});

export const messageSubmission = currentSubmission;
export const composerDraft = (sessionId: string | null): string => sessionId ? drafts()[sessionId] || '' : '';

export function setComposerDraft(sessionId: string | null, text: string): void {
  if (!sessionId) return;
  setDrafts((current) => ({ ...current, [sessionId]: text }));
}

export function clearComposerDraft(sessionId: string): void {
  setDrafts((current) => {
    if (!(sessionId in current)) return current;
    const next = { ...current };
    delete next[sessionId];
    return next;
  });
}

export function startMessageDelivery(commandId: string, text: string, sessionId: string, chatId: string): boolean {
  if (currentSubmission()) return false;
  setCurrentSubmission({ commandId, text, sessionId, chatId, phase: 'sending', detail: null, retryable: true, projected: false });
  clearComposerDraft(sessionId);
  return true;
}

export function acceptMessageDelivery(commandId: string): void {
  transition(commandId, (current) => ({ ...current, phase: 'accepted', detail: null }));
}

export function markMessageDeliveryUncertain(commandId: string): void {
  transition(commandId, (current) => ({
    ...current,
    phase: 'uncertain',
    detail: '服务器尚未确认结果。重新确认不会重复执行。',
    retryable: true,
  }));
}

export function failMessageDelivery(commandId: string, detail: string): void {
  const current = currentSubmission();
  if (!current || current.commandId !== commandId) return;
  if (current.projected) {
    setCurrentSubmission(null);
    return;
  }
  setCurrentSubmission({ ...current, phase: 'failed', detail, retryable: false });
}

/** Server crossed the no-redelivery barrier but cannot prove the outcome. */
export function blockUnknownMessageDelivery(commandId: string, detail?: string): void {
  transition(commandId, (current) => ({
    ...current,
    phase: 'delivery_unknown',
    detail: detail || '这条消息可能已经执行。为避免重复操作，系统已禁止重发和返回编辑。',
    retryable: false,
  }));
}

export function retryMessageDelivery(commandId: string): void {
  transition(commandId, (current) => ({ ...current, phase: 'sending', detail: null }));
}

export function completeMessageDelivery(commandId: string, status: unknown): boolean {
  const current = currentSubmission();
  if (!current || current.commandId !== commandId || (status !== 'committed' && status !== 'duplicate')) return false;
  if (current.projected) {
    setCurrentSubmission(null);
    return true;
  }
  setCurrentSubmission({
    ...current,
    phase: 'committed',
    detail: '服务器已确认，正在同步到对话记录。',
    retryable: false,
  });
  return true;
}

/** Only the exact durable Yjs identity may replace the local outbox item. */
export function reconcileMessageProjection(sourceCommandIds: ReadonlySet<string>): boolean {
  const current = currentSubmission();
  if (!current || !sourceCommandIds.has(current.commandId)) return false;
  if (current.phase === 'committed') setCurrentSubmission(null);
  else setCurrentSubmission({ ...current, projected: true });
  return true;
}

export function dismissFailedMessageDelivery(): void {
  const current = currentSubmission();
  if (!current || current.phase !== 'failed') return;
  setDrafts((currentDrafts) => currentDrafts[current.sessionId]
    ? currentDrafts
    : { ...currentDrafts, [current.sessionId]: current.text });
  setCurrentSubmission(null);
}

export function resetMessageDelivery(): void {
  setCurrentSubmission(null);
  setDrafts({});
}

function transition(commandId: string, update: (current: MessageSubmission) => MessageSubmission): void {
  const current = currentSubmission();
  if (!current || current.commandId !== commandId) return;
  setCurrentSubmission(update(current));
}
