import { SessionOwnershipConflictError } from "../kv/session-ownership-conflict-error";
import { sessionOwnershipKey } from "../kv/session-ownership-key";
import type { AtomicSessionOwnershipKv } from "../kv/types";
import type { SessionOwnership } from "./types";

export { sessionOwnershipKey } from "../kv/session-ownership-key";

/**
 * 包装底层原子 KV：同一实例内按 Session 去重，跨实例由 KV 的
 * `acquireIfAbsent`/`releaseIfOwner` 裁决。
 *
 * 每个 Manager 必须持有自己的 `AtomicSessionOwnership` 实例；共享同一个实例会让
 * 第二个 Manager 被误认为同一 owner。多个实例可以共享同一个 KV 驱动。
 */
export class AtomicSessionOwnership implements SessionOwnership {
  private readonly owner = crypto.randomUUID();
  private readonly owned = new Set<string>();

  constructor(private readonly kv: AtomicSessionOwnershipKv) {
    if (
      !kv ||
      typeof kv.acquireIfAbsent !== "function" ||
      typeof kv.releaseIfOwner !== "function"
    ) {
      throw new TypeError(
        "AtomicSessionOwnership requires acquireIfAbsent/releaseIfOwner",
      );
    }
  }

  /** 同一实例重复 acquire 同一 Session 是幂等的；跨实例冲突抛错。 */
  async acquire(sessionId: string): Promise<void> {
    if (typeof sessionId !== "string" || sessionId.length === 0) {
      throw new TypeError("Session id must be a non-empty string");
    }
    if (this.owned.has(sessionId)) return;
    const key = sessionOwnershipKey(sessionId);
    if (!(await this.kv.acquireIfAbsent(key, this.owner))) {
      throw new SessionOwnershipConflictError(key, sessionId);
    }
    this.owned.add(sessionId);
  }

  async release(sessionId: string): Promise<void> {
    if (!this.owned.has(sessionId)) {
      throw new Error(
        `Session ownership is not held by this ownership instance: ${sessionId}`,
      );
    }
    const key = sessionOwnershipKey(sessionId);
    if (!(await this.kv.releaseIfOwner(key, this.owner))) {
      throw new Error(
        `Cannot release session ownership held by another instance: ${key}`,
      );
    }
    this.owned.delete(sessionId);
  }

  /** 逆序释放；失败项保留，聚合上报，便于 shutdown 重试。 */
  async releaseAll(): Promise<void> {
    const failures: unknown[] = [];
    for (const sessionId of [...this.owned].reverse()) {
      try {
        await this.release(sessionId);
      } catch (error) {
        failures.push(error);
      }
    }
    if (failures.length > 0) {
      throw new AggregateError(
        failures,
        "Session ownership release is incomplete",
      );
    }
  }

  has(sessionId: string): boolean {
    return this.owned.has(sessionId);
  }

  get size(): number {
    return this.owned.size;
  }
}

export function requireSessionOwnership(
  value: unknown,
): asserts value is SessionOwnership {
  if (
    !value ||
    typeof value !== "object" ||
    typeof (value as Partial<SessionOwnership>).acquire !== "function" ||
    typeof (value as Partial<SessionOwnership>).release !== "function" ||
    typeof (value as Partial<SessionOwnership>).releaseAll !== "function"
  ) {
    throw new TypeError(
      "ChatManager requires sessionOwnership with acquire/release/releaseAll",
    );
  }
}

/**
 * Manager 本地 Session → 句柄索引。
 *
 * 它只回答“本 Manager 的哪个句柄正在加载/绑定/清理该 Session”，
 * 不替代共享 ownership：跨 Manager 的冲突仍由 `SessionOwnership` 决定。
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
