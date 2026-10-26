import { ChatNotReadyError } from "./errors";
import {
    asActionError,
    type ChatSessionIndex,
    type CleanupOutcome,
} from "./service-support";
import type { ChatDatabase, ChatDefaults } from "./types";

/** 候选 Session 的登记/acquire/释放依赖；由 ChatService 提供。 */
export interface CandidateContext {
    readonly chatId: string;
    readonly database: ChatDatabase;
    readonly path?: string;
    readonly defaults: ChatDefaults;
    readonly index: ChatSessionIndex;
    acquireSession(sessionId: string): Promise<void>;
    releaseOwnership(sessionId: string): Promise<void>;
}

interface CandidateRelease {
    /** 清理已确认且 ownership/索引均已释放。 */
    readonly released: boolean;
    /** 首个或聚合的清理失败原因；`released === true` 时为空。 */
    readonly error?: unknown;
}

/**
 * 单个 Session 候选的索引与 ownership 登记。
 *
 * `reserve` 是同步的：Session ID 一确定就写入索引，覆盖 path resolve、acquire、
 * bind、rename 与清理未确认期；只有清理确认后才移除。
 */
export class SessionCandidate {
    private reserved = false;
    private acquired = false;

    constructor(
        private readonly context: CandidateContext,
        readonly sessionId: string,
    ) {}

    /** 同步登记索引；返回冲突 owner（`null` 表示登记成功）。 */
    reserve(): string | null {
        const owner = this.context.index.reserve(
            this.context.chatId,
            this.sessionId,
        );
        if (owner === null) this.reserved = true;
        return owner;
    }

    /** 取得共享 ownership；失败原样抛出，由调用方按阶段映射错误。 */
    async acquire(): Promise<void> {
        await this.context.acquireSession(this.sessionId);
        this.acquired = true;
    }

    /**
     * 候选清理确认后才释放 ownership/索引。
     *
     * 清理未确认或释放失败都返回失败原因；绝不 `catch` 后静默返回成功。
     */
    async release(cleanup: CleanupOutcome): Promise<CandidateRelease> {
        if (!cleanup.confirmed) {
            return { released: false, error: cleanup.error };
        }
        const failures: unknown[] = [];
        if (this.acquired) {
            try {
                await this.context.releaseOwnership(this.sessionId);
                this.acquired = false;
            } catch (error) {
                failures.push(error);
            }
        }
        if (this.reserved && failures.length === 0) {
            try {
                this.context.index.release(
                    this.context.chatId,
                    this.sessionId,
                );
                this.reserved = false;
            } catch (error) {
                failures.push(error);
            }
        }
        if (failures.length === 0) return { released: true };
        return {
            released: false,
            error:
                failures.length === 1
                    ? failures[0]
                    : new AggregateError(
                          failures,
                          `Session ${this.sessionId} candidate release failed`,
                      ),
        };
    }
}

/**
 * 解析加载历史所需的 cwd。
 *
 * 持久 cwd 是权威；句柄/default path 只作兜底。查询失败保留动作错误语义。
 */
export async function resolveLoadPath(
    context: CandidateContext,
    sessionId: string,
    requestId: string,
): Promise<string> {
    let persistedCwd: string | undefined;
    try {
        const entry = await context.database.getSession(sessionId);
        persistedCwd = entry?.cwd;
    } catch (error) {
        throw asActionError(
            requestId,
            "session_lookup_failed",
            error,
            "rejected",
        );
    }
    const path = persistedCwd || context.path || context.defaults.path;
    if (!path) {
        throw new ChatNotReadyError(
            requestId,
            `Session ${sessionId} has no persisted cwd and no default path`,
        );
    }
    return path;
}
