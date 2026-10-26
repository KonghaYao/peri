import type { SessionOwnership, SessionOwnershipStorage } from "./types";

const OWNERSHIP_KEY_PREFIX = "ownership:";

/**
 * 共享协调域中的 Session 归属 key（内部符号，不从包入口导出）。
 *
 * base64url 只产生 `[A-Za-z0-9_-]`，不会被 unstorage 的 key normalize 改写；
 * key 只含 Session ID，不含 path 或 Chat ID。
 */
export function sessionOwnershipKey(sessionId: string): string {
  const bytes = new TextEncoder().encode(sessionId);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return `${OWNERSHIP_KEY_PREFIX}${btoa(binary)
    .replace(/\+/gu, "-")
    .replace(/\//gu, "_")
    .replace(/=+$/u, "")}`;
}

interface SessionOwnershipRecord {
  readonly sessionId: string;
  readonly owner: string;
}

/**
 * 本实例要绑定的 Session 已被其他 owner 登记。
 *
 * 内部错误：Chat 调用方只会收到 `SessionBindingConflictError`。
 */
export class SessionOwnershipConflictError extends Error {
  constructor(readonly sessionId: string) {
    super(`Session is already owned by another instance: ${sessionId}`);
    this.name = "SessionOwnershipConflictError";
  }
}

/**
 * 基于 3 方法存储端口的 Session ownership（内部实现）。
 *
 * 读后写不是原子操作：两个实例同时读到空记录时会都写成功。SDK 不检测这种
 * 竞态；同一 Session 单实例绑定由路由层串行化保证。这里只负责记录读取、
 * owner 校验与幂等重试。
 */
export class KvSessionOwnership implements SessionOwnership {
  private readonly owner = crypto.randomUUID();
  private readonly owned = new Set<string>();

  constructor(private readonly storage: SessionOwnershipStorage) {
    if (
      !storage ||
      typeof storage.getItem !== "function" ||
      typeof storage.setItem !== "function" ||
      typeof storage.removeItem !== "function"
    ) {
      throw new TypeError(
        "Session ownership requires storage with getItem/setItem/removeItem",
      );
    }
  }

  /** 同一实例重复 acquire 幂等；记录属于其他 owner 时冲突且不覆盖。 */
  async acquire(sessionId: string): Promise<void> {
    assertSessionId(sessionId);
    if (this.owned.has(sessionId)) return;
    const key = sessionOwnershipKey(sessionId);
    const value = await this.storage.getItem(key);
    if (value !== null && value !== undefined) {
      const record = parseRecord(value, sessionId);
      if (record.owner !== this.owner) {
        throw new SessionOwnershipConflictError(sessionId);
      }
      this.owned.add(sessionId);
      return;
    }
    await this.storage.setItem(key, { sessionId, owner: this.owner });
    this.owned.add(sessionId);
  }

  /**
   * owner 匹配才删除；记录缺失或已属于其他 owner 时只清理本地状态。
   *
   * 读/删失败向上抛出并保留本地 owned，供调用方重试。
   */
  async release(sessionId: string): Promise<void> {
    if (!this.owned.has(sessionId)) {
      throw new Error(
        `Session ownership is not held by this ownership instance: ${sessionId}`,
      );
    }
    const key = sessionOwnershipKey(sessionId);
    const value = await this.storage.getItem(key);
    if (value !== null && value !== undefined) {
      const record = parseRecord(value, sessionId);
      if (record.owner === this.owner) {
        await this.storage.removeItem(key);
      }
    }
    this.owned.delete(sessionId);
  }
}

/**
 * 路由层清理入口：删除指定 Session 的归属记录，不校验 owner。
 *
 * 只能在旧实例已停止或已被 fence 后调用；误调用可能造成双控制者。
 * 换绑优先让旧实例优雅 release，本函数用于崩溃恢复或强制换绑。
 */
export async function clearSessionOwnership(
  storage: SessionOwnershipStorage,
  sessionId: string,
): Promise<void> {
  if (!storage || typeof storage.removeItem !== "function") {
    throw new TypeError(
      "clearSessionOwnership requires storage with removeItem",
    );
  }
  assertSessionId(sessionId);
  await storage.removeItem(sessionOwnershipKey(sessionId));
}

function assertSessionId(sessionId: string): void {
  if (typeof sessionId !== "string" || sessionId.length === 0) {
    throw new TypeError("Session id must be a non-empty string");
  }
}

/** 非法记录 fail closed：不当作空记录，也不覆盖。 */
function parseRecord(value: unknown, sessionId: string): SessionOwnershipRecord {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError(
      `Invalid session ownership record for ${sessionId}: expected an object`,
    );
  }
  const record = value as Record<string, unknown>;
  if (
    typeof record.sessionId !== "string" ||
    typeof record.owner !== "string" ||
    record.owner.length === 0
  ) {
    throw new TypeError(
      `Invalid session ownership record for ${sessionId}: missing sessionId/owner`,
    );
  }
  if (record.sessionId !== sessionId) {
    throw new TypeError(
      `Invalid session ownership record for ${sessionId}: sessionId mismatch`,
    );
  }
  return { sessionId: record.sessionId, owner: record.owner };
}

/**
 * Manager 本地 Session → 句柄索引。
 *
 * 它只回答“本 Manager 的哪个句柄正在加载/绑定/清理该 Session”，
 * 不替代共享 ownership：跨 Manager 的冲突由 ownership 记录决定。
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
