import { sessionOwnershipKey } from "../src/chat/ownership";
import type { KvStorage } from "../src/chat/types";

/**
 * 测试用 Session ownership 存储：实现生产 3 方法端口，并在内存中记录 key。
 *
 * Manager 内部会据此构造 ownership；测试通过 `has`/`size` 观察结果，
 * 通过 `failRead`/`failWrite`/`failRelease` 注入存储故障。
 */
export class OwnershipProbe implements KvStorage {
    private readonly values = new Map<string, unknown>();
    private readonly readFailures = new Map<string, Error>();
    private readonly writeFailures = new Map<string, Error>();
    private readonly writeAfterFailures = new Map<string, Error>();
    private readonly releaseFailures = new Map<string, Error>();

    async getItem(key: string): Promise<unknown> {
        const failure = this.readFailures.get(key);
        if (failure) throw failure;
        return this.values.get(key) ?? null;
    }

    async setItem(key: string, value: unknown): Promise<void> {
        const failure = this.writeFailures.get(key);
        if (failure) throw failure;
        this.values.set(key, structuredClone(value));
        const afterFailure = this.writeAfterFailures.get(key);
        if (afterFailure) throw afterFailure;
    }

    async removeItem(key: string): Promise<void> {
        const failure = this.releaseFailures.get(key);
        if (failure) throw failure;
        this.values.delete(key);
    }

    has(sessionId: string): boolean {
        return this.values.has(sessionOwnershipKey(sessionId));
    }

    get size(): number {
        return this.values.size;
    }

    /** 观察原始记录，用于断言 key/value 形状。 */
    value(sessionId: string): unknown {
        return this.values.get(sessionOwnershipKey(sessionId));
    }

    /** 直接写入任意记录，用于模拟其他实例或非法记录。 */
    put(sessionId: string, value: unknown): void {
        this.values.set(sessionOwnershipKey(sessionId), value);
    }

    /** 直接删除记录，不经过故障注入。 */
    drop(sessionId: string): void {
        this.values.delete(sessionOwnershipKey(sessionId));
    }

    /** 模拟另一个实例已持有 Session。 */
    hold(sessionId: string, owner = "other-owner"): void {
        this.put(sessionId, { sessionId, owner });
    }

    failRead(
        sessionId: string,
        error: Error = new Error("ownership read failed"),
    ): void {
        this.readFailures.set(sessionOwnershipKey(sessionId), error);
    }

    failWrite(
        sessionId: string,
        error: Error = new Error("ownership write failed"),
    ): void {
        this.writeFailures.set(sessionOwnershipKey(sessionId), error);
    }

    /** 模拟写入已落地但响应丢失：记录存在，调用方收到失败。 */
    failWriteAfterWrite(
        sessionId: string,
        error: Error = new Error("ownership write response lost"),
    ): void {
        this.writeAfterFailures.set(sessionOwnershipKey(sessionId), error);
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

    /** 清除该 Session 的全部故障注入。 */
    clearFailure(sessionId: string): void {
        const key = sessionOwnershipKey(sessionId);
        this.readFailures.delete(key);
        this.writeFailures.delete(key);
        this.writeAfterFailures.delete(key);
        this.releaseFailures.delete(key);
    }
}
