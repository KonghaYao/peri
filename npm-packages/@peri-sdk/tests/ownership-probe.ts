import { sessionOwnershipKey } from "../src/kv/session-ownership-key";
import type { AtomicSessionOwnershipKv } from "../src/kv/types";

/**
 * 测试用原子 KV：直接实现生产接缝，并在内存中记录 ownership key。
 *
 * Manager 内部会据此构造 ownership；测试通过 `has`/`size` 观察结果，
 * 不再依赖或注入 Manager 内部的协调器实例。
 */
export class OwnershipProbe implements AtomicSessionOwnershipKv {
    private readonly owners = new Map<string, string>();
    private readonly releaseFailures = new Map<string, Error>();

    async acquireIfAbsent(key: string, owner: string): Promise<boolean> {
        if (this.owners.has(key)) return false;
        this.owners.set(key, owner);
        return true;
    }

    async releaseIfOwner(key: string, owner: string): Promise<boolean> {
        const failure = this.releaseFailures.get(key);
        if (failure) throw failure;
        if (this.owners.get(key) !== owner) return false;
        this.owners.delete(key);
        return true;
    }

    has(sessionId: string): boolean {
        return this.owners.has(sessionOwnershipKey(sessionId));
    }

    get size(): number {
        return this.owners.size;
    }

    /** 模拟另一个实例已持有 Session。 */
    hold(sessionId: string, owner = "other-owner"): void {
        this.owners.set(sessionOwnershipKey(sessionId), owner);
    }

    /** 让该 Session 的释放失败，用于关闭失败/重试路径。 */
    failRelease(
        sessionId: string,
        error: Error = new Error("ownership release failed"),
    ): void {
        this.releaseFailures.set(sessionOwnershipKey(sessionId), error);
    }

    clearReleaseFailure(sessionId: string): void {
        this.releaseFailures.delete(sessionOwnershipKey(sessionId));
    }
}
