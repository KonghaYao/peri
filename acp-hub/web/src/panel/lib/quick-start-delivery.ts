import { createSignal } from 'solid-js';

export type QuickStartPhase = 'creating' | 'accepted' | 'uncertain' | 'failed';

export interface QuickStartSubmission {
  commandId: string;
  projectId: string;
  text: string;
  phase: QuickStartPhase;
  detail: string | null;
  retryable: boolean;
}

export interface QuickStartActivation {
  commandId: string;
  sessionId: string;
  chatId: string;
  text: string;
}

const [currentSubmission, setCurrentSubmission] = createSignal<QuickStartSubmission | null>(null);

export const quickStartSubmission = currentSubmission;

export function startQuickStart(commandId: string, projectId: string, text: string): boolean {
  if (currentSubmission()) return false;
  setCurrentSubmission({ commandId, projectId, text, phase: 'creating', detail: null, retryable: true });
  return true;
}

export function acceptQuickStart(commandId: string): void {
  transition(commandId, (current) => ({ ...current, phase: 'accepted', detail: null }));
}

export function markQuickStartUncertain(commandId: string): void {
  transition(commandId, (current) => ({
    ...current,
    phase: 'uncertain',
    detail: '会话创建结果尚未确认。重新确认会复用同一请求，不会创建重复会话。',
    retryable: true,
  }));
}

export function failQuickStart(commandId: string, detail: string): void {
  transition(commandId, (current) => ({ ...current, phase: 'failed', detail, retryable: false }));
}

export function retryQuickStartDelivery(commandId: string): void {
  transition(commandId, (current) => ({ ...current, phase: 'creating', detail: null }));
}

export function completeQuickStart(commandId: string, status: unknown, sessionId: unknown, chatId: unknown): QuickStartActivation | null {
  const current = currentSubmission();
  if (!current || !canActivate(current, commandId, status, sessionId, chatId)) return null;
  setCurrentSubmission(null);
  return { commandId, sessionId: sessionId as string, chatId: chatId as string, text: current.text };
}

export function settleLateQuickStart(commandId: string, status: unknown, sessionId: unknown, chatId: unknown): boolean {
  const current = currentSubmission();
  if (!current || !canActivate(current, commandId, status, sessionId, chatId)) return false;
  setCurrentSubmission({
    ...current,
    phase: 'failed',
    detail: '会话已在服务器创建，但首条消息没有自动发送。请从侧边栏打开该会话后重新发送保留的原文。',
    retryable: false,
  });
  return true;
}

export function dismissFailedQuickStart(): void {
  if (currentSubmission()?.phase === 'failed') setCurrentSubmission(null);
}

export function resetQuickStart(): void {
  setCurrentSubmission(null);
}

function transition(commandId: string, update: (current: QuickStartSubmission) => QuickStartSubmission): void {
  const current = currentSubmission();
  if (!current || current.commandId !== commandId) return;
  setCurrentSubmission(update(current));
}

function canActivate(
  current: QuickStartSubmission,
  commandId: string,
  status: unknown,
  sessionId: unknown,
  chatId: unknown,
): boolean {
  return current.commandId === commandId
    && (status === 'committed' || status === 'duplicate')
    && typeof sessionId === 'string'
    && !!sessionId
    && typeof chatId === 'string'
    && !!chatId;
}
