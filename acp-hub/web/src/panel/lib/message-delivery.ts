import { createSignal } from 'solid-js';

export type MessageDeliveryPhase = 'sending' | 'accepted' | 'uncertain' | 'failed';

export interface MessageSubmission {
  commandId: string;
  text: string;
  sessionId: string;
  chatId: string;
  phase: MessageDeliveryPhase;
  detail: string | null;
  retryable: boolean;
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
  setCurrentSubmission({ commandId, text, sessionId, chatId, phase: 'sending', detail: null, retryable: true });
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
  }), true);
}

export function failMessageDelivery(commandId: string, detail: string): void {
  transition(commandId, (current) => ({ ...current, phase: 'failed', detail, retryable: false }), true);
}

export function retryMessageDelivery(commandId: string): void {
  transition(commandId, (current) => ({ ...current, phase: 'sending', detail: null }));
}

export function completeMessageDelivery(commandId: string, status: unknown): boolean {
  const current = currentSubmission();
  if (!current || current.commandId !== commandId || (status !== 'committed' && status !== 'duplicate')) return false;
  if (drafts()[current.sessionId] === current.text) clearComposerDraft(current.sessionId);
  setCurrentSubmission(null);
  return true;
}

export function dismissFailedMessageDelivery(): void {
  if (currentSubmission()?.phase === 'failed') setCurrentSubmission(null);
}

export function resetMessageDelivery(): void {
  setCurrentSubmission(null);
  setDrafts({});
}

function transition(commandId: string, update: (current: MessageSubmission) => MessageSubmission, restoreDraft = false): void {
  const current = currentSubmission();
  if (!current || current.commandId !== commandId) return;
  const next = update(current);
  if (restoreDraft) setDrafts((currentDrafts) => currentDrafts[next.sessionId]
    ? currentDrafts
    : { ...currentDrafts, [next.sessionId]: next.text });
  setCurrentSubmission(next);
}
