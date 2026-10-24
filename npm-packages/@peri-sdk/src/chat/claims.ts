import { SessionClaimConflictError } from "../kv/session-claim-conflict-error";
import { sessionClaimKey } from "../kv/session-claim-key";
import type { AtomicSessionClaimKv } from "../kv/types";
import type { SessionClaims } from "./types";

export { sessionClaimKey } from "../kv/session-claim-key";

/**
 * 包装底层原子 KV：同一实例内按 Session 去重，跨实例由 KV 的
 * `claimIfAbsent`/`releaseIfOwner` 裁决。
 *
 * 每个 Manager 必须持有自己的 `AtomicSessionClaims` 实例；共享同一个实例会让
 * 第二个 Manager 被误认为同一 owner。多个实例可以共享同一个 KV 驱动。
 */
export class AtomicSessionClaims implements SessionClaims {
  private readonly owner = crypto.randomUUID();
  private readonly claimed = new Set<string>();

  constructor(private readonly kv: AtomicSessionClaimKv) {
    if (
      !kv ||
      typeof kv.claimIfAbsent !== "function" ||
      typeof kv.releaseIfOwner !== "function"
    ) {
      throw new TypeError(
        "AtomicSessionClaims requires claimIfAbsent/releaseIfOwner",
      );
    }
  }

  /** 同一实例重复 claim 同一 Session 是幂等的；跨实例冲突抛错。 */
  async claim(sessionId: string): Promise<void> {
    if (typeof sessionId !== "string" || sessionId.length === 0) {
      throw new TypeError("Session id must be a non-empty string");
    }
    if (this.claimed.has(sessionId)) return;
    const key = sessionClaimKey(sessionId);
    if (!(await this.kv.claimIfAbsent(key, this.owner))) {
      throw new SessionClaimConflictError(key, sessionId);
    }
    this.claimed.add(sessionId);
  }

  async release(sessionId: string): Promise<void> {
    if (!this.claimed.has(sessionId)) {
      throw new Error(
        `Session claim is not owned by this claims instance: ${sessionId}`,
      );
    }
    const key = sessionClaimKey(sessionId);
    if (!(await this.kv.releaseIfOwner(key, this.owner))) {
      throw new Error(
        `Cannot release session claim owned by another instance: ${key}`,
      );
    }
    this.claimed.delete(sessionId);
  }

  /** 逆序释放；失败项保留，聚合上报，便于 shutdown 重试。 */
  async releaseAll(): Promise<void> {
    const failures: unknown[] = [];
    for (const sessionId of [...this.claimed].reverse()) {
      try {
        await this.release(sessionId);
      } catch (error) {
        failures.push(error);
      }
    }
    if (failures.length > 0) {
      throw new AggregateError(
        failures,
        "Session claim release is incomplete",
      );
    }
  }

  has(sessionId: string): boolean {
    return this.claimed.has(sessionId);
  }

  get size(): number {
    return this.claimed.size;
  }
}

export function requireSessionClaims(
  value: unknown,
): asserts value is SessionClaims {
  if (
    !value ||
    typeof value !== "object" ||
    typeof (value as Partial<SessionClaims>).claim !== "function" ||
    typeof (value as Partial<SessionClaims>).release !== "function" ||
    typeof (value as Partial<SessionClaims>).releaseAll !== "function"
  ) {
    throw new TypeError(
      "ChatManager requires sessionClaims with claim/release/releaseAll",
    );
  }
}

/**
 * Manager 本地 Session → 句柄索引。
 *
 * 它只回答“本 Manager 的哪个句柄正在加载/绑定/清理该 Session”，
 * 不替代共享 claim：跨 Manager 的冲突仍由 `SessionClaims` 决定。
 * 条目在 binding 开始时写入，确认清理后才移除。
 */
export class LocalSessionIndex {
  private readonly owners = new Map<string, string>();

  /** 返回已有 owner；`null` 表示登记成功。同一 chatId 重复登记是幂等的。 */
  reserve(sessionId: string, chatId: string): string | null {
    const owner = this.owners.get(sessionId);
    if (owner !== undefined && owner !== chatId) return owner;
    this.owners.set(sessionId, chatId);
    return null;
  }

  /** 绑定提交后移除旧 Session 条目，并确保新条目指向该句柄。 */
  commit(
    chatId: string,
    previousSessionId: string | null,
    sessionId: string,
  ): void {
    if (
      previousSessionId !== null &&
      previousSessionId !== sessionId &&
      this.owners.get(previousSessionId) === chatId
    ) {
      this.owners.delete(previousSessionId);
    }
    this.owners.set(sessionId, chatId);
  }

  release(sessionId: string, chatId: string): void {
    if (this.owners.get(sessionId) === chatId) this.owners.delete(sessionId);
  }

  ownerOf(sessionId: string): string | undefined {
    return this.owners.get(sessionId);
  }

  /** 移除该句柄的全部条目；返回被移除的 Session ID。 */
  removeChat(chatId: string): string[] {
    const removed: string[] = [];
    for (const [sessionId, owner] of this.owners) {
      if (owner !== chatId) continue;
      this.owners.delete(sessionId);
      removed.push(sessionId);
    }
    return removed;
  }

  clear(): void {
    this.owners.clear();
  }

  get size(): number {
    return this.owners.size;
  }
}
