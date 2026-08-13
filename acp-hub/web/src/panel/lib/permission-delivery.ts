import { createSignal } from 'solid-js';

export type PermissionDecision = 'allow' | 'deny';
export type PermissionDecisionPhase = 'pending' | 'uncertain';

export interface PermissionDecisionState {
  commandId: string;
  permissionId: string;
  decision: PermissionDecision;
  phase: PermissionDecisionPhase;
  retryable: boolean;
}

const [decisions, setDecisions] = createSignal<Map<string, PermissionDecisionState>>(new Map());

export const permissionDecisions = decisions;

export function startPermissionDecision(commandId: string, permissionId: string, decision: PermissionDecision): boolean {
  if (!permissionId || decisions().has(permissionId)) return false;
  setDecisions((current) => new Map(current).set(permissionId, { commandId, permissionId, decision, phase: 'pending', retryable: false }));
  return true;
}

export function markPermissionDecisionUncertain(commandId: string, retryable = false): void {
  transition(commandId, (current) => ({ ...current, phase: 'uncertain', retryable }));
}

export function retryPermissionDecision(commandId: string): void {
  transition(commandId, (current) => ({ ...current, phase: 'pending', retryable: false }));
}

export function failPermissionDecision(commandId: string): void {
  const entry = byCommand(commandId);
  if (entry) remove(entry.permissionId);
}

export function retainProjectedPermissions(permissionIds: ReadonlySet<string>): void {
  setDecisions((current) => {
    const retained = new Map([...current].filter(([permissionId]) => permissionIds.has(permissionId)));
    return retained.size === current.size ? current : retained;
  });
}

export function resetPermissionDecisions(): void {
  setDecisions(new Map());
}

function transition(commandId: string, update: (current: PermissionDecisionState) => PermissionDecisionState): void {
  const entry = byCommand(commandId);
  if (!entry) return;
  setDecisions((current) => new Map(current).set(entry.permissionId, update(entry)));
}

function byCommand(commandId: string): PermissionDecisionState | undefined {
  return [...decisions().values()].find((current) => current.commandId === commandId);
}

function remove(permissionId: string): void {
  setDecisions((current) => {
    if (!current.has(permissionId)) return current;
    const next = new Map(current);
    next.delete(permissionId);
    return next;
  });
}
