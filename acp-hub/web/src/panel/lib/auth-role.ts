export type PrincipalRole = 'full' | 'read-only' | null;
export const canMutate = (role: PrincipalRole): boolean => role === 'full';
export const parsePrincipal = (value: unknown): PrincipalRole => {
  if (!value || typeof value !== 'object') return null;
  const role = (value as { role?: unknown }).role;
  return role === 'full' || role === 'read-only' ? role : null;
};
