/**
 * 共享协调域中的 Session key。
 *
 * 沿用以 `peri:managed-agent:session:` 开头的形态，保证升级期间新旧 SDK
 * 仍竞争同一把锁；key 不含 Sandbox/Agent/path 维度。
 */
export function sessionClaimKey(sessionId: string): string {
  return `peri:managed-agent:session:${encodeURIComponent(sessionId)}`;
}
