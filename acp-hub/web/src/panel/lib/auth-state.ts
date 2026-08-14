import { createSignal } from 'solid-js';
import { canMutate, type PrincipalRole } from './auth-role';

export interface AuthInvalidation {
  id: number;
  reason: string;
}

const [principal, setPrincipal] = createSignal<PrincipalRole>(null);
const [invalidation, setInvalidation] = createSignal<AuthInvalidation | null>(null);
let invalidationSequence = 0;

export const principalRole = principal;
export const setPrincipalRole = setPrincipal;
export const readOnly = (): boolean => !canMutate(principal());
export const authInvalidation = invalidation;

export function installPrincipalRole(role: PrincipalRole): void {
  setPrincipal(role);
}

export function publishAuthInvalidation(reason: string): void {
  setPrincipal(null);
  setInvalidation({ id: ++invalidationSequence, reason });
}

export function clearAuthInvalidation(): void {
  setInvalidation(null);
}
