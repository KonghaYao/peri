/**
 * 共享协调域中的 Session key。
 *
 * 沿用既有协调域前缀，保证升级期间新旧实例仍竞争同一把锁；key 不含 path 维度。
 * 前缀是存储协调标识，不是公开符号；改名会破坏跨版本 claim 互斥，需单独迁移。
 */
export function sessionClaimKey(sessionId: string): string {
  return `peri:managed-agent:session:${encodeURIComponent(sessionId)}`;
}
