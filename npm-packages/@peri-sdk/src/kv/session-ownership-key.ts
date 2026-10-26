/**
 * 共享协调域中的 Session 归属 key。
 *
 * 存储 key 字符串（含 `peri:managed-agent:session:` 前缀）保持不变，保证升级期间
 * 新旧实例仍竞争同一把锁；只改函数名，不改 key 形状。key 不含 path 维度。
 * 前缀是存储协调标识，不是公开符号；改名会破坏跨版本 ownership 互斥，需单独迁移。
 */
export function sessionOwnershipKey(sessionId: string): string {
  return `peri:managed-agent:session:${encodeURIComponent(sessionId)}`;
}
