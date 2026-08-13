import { createSignal } from 'solid-js';

export type RuntimeControlKind = 'cancel' | 'close';
export type RuntimeControlPhase = 'sending' | 'accepted' | 'uncertain' | 'confirmed' | 'failed';

export interface RuntimeControlSubmission {
  commandId: string;
  chatId: string;
  kind: RuntimeControlKind;
  phase: RuntimeControlPhase;
  detail: string | null;
  retryable: boolean;
}

const [controls, setControls] = createSignal<Record<string, RuntimeControlSubmission>>({});

export const runtimeControlFor = (chatId: string | null): RuntimeControlSubmission | null =>
  chatId ? controls()[chatId] || null : null;

export const runtimeControlBusy = (chatId: string | null, kind?: RuntimeControlKind): boolean => {
  const control = runtimeControlFor(chatId);
  return !!control && (!kind || control.kind === kind) && control.phase !== 'failed';
};

export function startRuntimeControl(commandId: string, chatId: string, kind: RuntimeControlKind): boolean {
  const existing = controls()[chatId];
  if (existing && existing.phase !== 'failed') return false;
  setControls((current) => ({
    ...current,
    [chatId]: { commandId, chatId, kind, phase: 'sending', detail: null, retryable: true },
  }));
  return true;
}

export function acceptRuntimeControl(commandId: string): void {
  transition(commandId, (current) => ({ ...current, phase: 'accepted', detail: null }));
}

export function markRuntimeControlUncertain(commandId: string): void {
  transition(commandId, (current) => ({
    ...current,
    phase: 'uncertain',
    detail: current.kind === 'cancel'
      ? '停止请求可能仍在执行。请使用原请求重新确认，不要连续提交新的停止操作。'
      : '运行实例可能已经关闭。会话仍会保存在项目中，请使用原请求重新确认。',
    retryable: true,
  }));
}

export function failRuntimeControl(commandId: string, detail: string): void {
  transition(commandId, (current) => ({ ...current, phase: 'failed', detail, retryable: false }));
}

export function retryRuntimeControl(commandId: string): void {
  transition(commandId, (current) => ({ ...current, phase: 'sending', detail: null }));
}

export function confirmRuntimeControl(commandId: string, status: unknown): boolean {
  if (status !== 'committed' && status !== 'duplicate') return false;
  return transition(commandId, (current) => ({ ...current, phase: 'confirmed', detail: null, retryable: false }));
}

export function reconcileRuntimeControl(chatId: string, turnActive: boolean, terminal: boolean): void {
  const current = controls()[chatId];
  if (!current) return;
  if ((current.kind === 'cancel' && (!turnActive || terminal)) || (current.kind === 'close' && terminal)) {
    remove(chatId);
  }
}

export function resetRuntimeControls(): void {
  setControls({});
}

function transition(commandId: string, update: (current: RuntimeControlSubmission) => RuntimeControlSubmission): boolean {
  const entry = Object.values(controls()).find((current) => current.commandId === commandId);
  if (!entry) return false;
  setControls((current) => ({ ...current, [entry.chatId]: update(entry) }));
  return true;
}

function remove(chatId: string): void {
  setControls((current) => {
    if (!(chatId in current)) return current;
    const next = { ...current };
    delete next[chatId];
    return next;
  });
}
