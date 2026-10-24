/**
 * 会话占用的原子 KV 能力。
 *
 * 共享协调域只按 Session ID 建立 claim；调用方不得用 path、Sandbox 或 Agent ID
 * 组成第二套 key 绕过这里。驱动必须真正实现 `claimIfAbsent`/`releaseIfOwner`
 * （普通 unstorage Storage 没有 compare-and-set，会被拒绝）。
 */
export interface AtomicSessionClaimKv {
  claimIfAbsent(key: string, owner: string): Promise<boolean>;
  releaseIfOwner(key: string, owner: string): Promise<boolean>;
}

/**
 * Phase 2 过渡别名：旧 `ManagedAgents`/`Agent` 仍在 `src/agent/**`、
 * `src/execution/**` 与主入口中引用该名字，而它们不在 Stream B 的写入范围。
 * Phase 4 删除旧层时移除，不新增第二套实现。
 */
export type AtomicManagedAgentKv = AtomicSessionClaimKv;

export function requireAtomicKv(
  value: unknown,
): asserts value is AtomicSessionClaimKv {
  if (
    !value ||
    typeof value !== "object" ||
    typeof (value as Partial<AtomicSessionClaimKv>).claimIfAbsent !==
      "function" ||
    typeof (value as Partial<AtomicSessionClaimKv>).releaseIfOwner !==
      "function"
  ) {
    throw new TypeError(
      "Session claims require an atomic claimIfAbsent/releaseIfOwner KV adapter",
    );
  }
}
