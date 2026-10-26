/**
 * 会话占用的原子 KV 能力。
 *
 * 共享协调域只按 Session ID 建立 claim；调用方不得用 path 或 Chat ID
 * 组成第二套 key 绕过这里。驱动必须真正实现 `claimIfAbsent`/`releaseIfOwner`
 * （普通 unstorage Storage 没有 compare-and-set，会被拒绝）。
 */
export interface AtomicSessionClaimKv {
  claimIfAbsent(key: string, owner: string): Promise<boolean>;
  releaseIfOwner(key: string, owner: string): Promise<boolean>;
}
