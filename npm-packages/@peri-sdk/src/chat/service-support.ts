import { notificationBytes } from "../transport/notification-budget";
import type { PeriWasmHostLease } from "../wasm/host-lifecycle";
import {
    ChatActionError,
    ChatBusyError,
    ChatClosedError,
    ChatNotReadyError,
    ChatViewReplayOverflowError,
    SessionBindingConflictError,
    SessionCreationError,
} from "./errors";
import type { ChatFact, ChatFactEnvelope } from "./facts";
import type {
    ChatBindingTarget,
    ChatDatabase,
    ChatDefaults,
    ChatHost,
    ChatLifecycleOptions,
    PeriInstancePort,
    SessionClaims,
} from "./types";

export const MAX_FACT_COUNT = 4096;
export const MAX_FACT_BYTES = 4 * 1024 * 1024;

/** 关闭/启动预算的单一权威默认值；调用方只按需覆盖，不各自发明超时。 */
export const DEFAULT_CHAT_LIFECYCLE: Required<ChatLifecycleOptions> =
    Object.freeze({
        closeDrainTimeoutMs: 3000,
        startupSettleTimeoutMs: 5000,
        activationCloseTimeoutMs: 5000,
        shutdownChatTimeoutMs: 15000,
    });

/** 归一化生命周期预算；非法值直接拒绝，避免关闭退化为无界等待。 */
export function resolveChatLifecycle(
    options?: ChatLifecycleOptions,
): Required<ChatLifecycleOptions> {
    const resolved = { ...DEFAULT_CHAT_LIFECYCLE, ...options };
    for (const [name, value] of Object.entries(resolved)) {
        if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
            throw new TypeError(
                `ChatLifecycleOptions.${name} must be a finite non-negative number`,
            );
        }
    }
    return resolved;
}

/** ChatService 构造 PeriInstance 时提供的装配上下文（Phase 4 接入真实实现）。 */
export interface ChatInstanceContext {
    readonly chatId: string;
    readonly path: string;
    /** Manager 构造时的纯 JSON 快照。 */
    readonly agentConfig: unknown;
    readonly defaults: ChatDefaults;
    readonly host: ChatHost;
    readonly signal: AbortSignal;
    readonly now?: () => string;
    readonly database: ChatDatabase;
    readonly lifecycle: Required<ChatLifecycleOptions>;
    /** 仅 host.kind === "wasm" 可用；租约由 PeriInstance 关闭并检查 confirmed。 */
    acquireWasmHost(signal?: AbortSignal): Promise<PeriWasmHostLease>;
}

export type ChatInstanceFactory = (
    context: ChatInstanceContext,
) => PeriInstancePort;

/** Manager 提供的本地 Session → 句柄索引；不替代共享 claim。 */
export interface ChatSessionIndex {
    /** 返回冲突句柄 id；`null` 表示登记成功。 */
    reserve(chatId: string, sessionId: string): string | null;
    commit(
        chatId: string,
        previousSessionId: string | null,
        sessionId: string,
    ): void;
    release(chatId: string, sessionId: string): void;
}

export interface ChatServiceOptions {
    readonly id: string;
    /** createChat({path}) 的句柄级 path；新会话优先使用，加载历史仅兜底。 */
    readonly path?: string;
    readonly defaults: ChatDefaults;
    readonly agentConfig: unknown;
    readonly host: ChatHost;
    readonly database: ChatDatabase;
    readonly claims: SessionClaims;
    readonly index: ChatSessionIndex;
    readonly lifecycle?: ChatLifecycleOptions;
    /** Manager 默认装配真实 PeriInstance；测试注入 fake port。 */
    readonly instanceFactory: ChatInstanceFactory;
    acquireWasmHost(signal?: AbortSignal): Promise<PeriWasmHostLease>;
    onClosed(chatId: string): void;
    readonly now?: () => string;
}

export interface Activation {
    readonly port: PeriInstancePort;
    readonly controller: AbortController;
    readonly pending: ChatFact[];
    detach: () => void;
    live: boolean;
    abandoned: boolean;
}

export interface CleanupOutcome {
    readonly confirmed: boolean;
    readonly error?: unknown;
}

/** 单个绑定代次的有界事实日志；溢出后只保留 live fan-out。 */
export class FactLog {
    private readonly envelopes: ChatFactEnvelope[] = [];
    private bytes = 0;
    private nextSequence = 1;
    overflowed = false;

    constructor(
        private readonly chatId: string,
        private readonly target: ChatBindingTarget,
    ) {}

    append(fact: ChatFact): ChatFactEnvelope {
        const envelope: ChatFactEnvelope = Object.freeze({
            chatId: this.chatId,
            sessionId: this.target.sessionId,
            bindingRevision: this.target.bindingRevision,
            sequence: this.nextSequence++,
            fact,
        });
        if (!this.overflowed) {
            const size = notificationBytes(fact, MAX_FACT_BYTES);
            if (
                this.envelopes.length >= MAX_FACT_COUNT ||
                this.bytes + size > MAX_FACT_BYTES
            ) {
                this.overflowed = true;
            } else {
                this.envelopes.push(envelope);
                this.bytes += size;
            }
        }
        return envelope;
    }

    replay(): readonly ChatFactEnvelope[] {
        if (this.overflowed) {
            throw new ChatViewReplayOverflowError(
                "Chat fact log overflowed; a new view must wait for a new binding",
            );
        }
        return [...this.envelopes];
    }
}

export function messageOf(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
}

/** 同一动作被合并/重放时，把错误归到当前调用方的 requestId。 */
export function withRequestId(error: unknown, requestId: string): unknown {
    if (!(error instanceof ChatActionError) || error.requestId === requestId)
        return error;
    if (error instanceof SessionCreationError) {
        return new SessionCreationError(
            requestId,
            error.createdSessionId,
            error.stage,
            error.message,
            { cause: error },
        );
    }
    if (error instanceof SessionBindingConflictError) {
        return new SessionBindingConflictError(
            requestId,
            error.message,
            error.ownerChatId,
        );
    }
    if (error instanceof ChatBusyError) {
        return new ChatBusyError(requestId, error.message, { cause: error });
    }
    if (error instanceof ChatNotReadyError) {
        return new ChatNotReadyError(requestId, error.message, {
            cause: error,
        });
    }
    if (error instanceof ChatClosedError) {
        return new ChatClosedError(requestId, error.message, { cause: error });
    }
    return new ChatActionError(
        requestId,
        error.code,
        error.message,
        error.disposition,
        { cause: error },
    );
}

export function asActionError(
    requestId: string,
    code: string,
    error: unknown,
    disposition: ChatActionError["disposition"],
): ChatActionError {
    if (error instanceof ChatActionError)
        return withRequestId(error, requestId) as ChatActionError;
    return new ChatActionError(requestId, code, messageOf(error), disposition, {
        cause: error,
    });
}

/** 原始失败 + 清理失败；清理原因不能被吞掉或覆盖。 */
export function withCleanupFailure(
    error: unknown,
    cleanupError: unknown,
): unknown {
    if (cleanupError === undefined) return error;
    return new AggregateError(
        [error, cleanupError],
        `${messageOf(error)}; cleanup also failed`,
    );
}

/**
 * 绑定失败的错误映射：清理未确认时不得向调用方声称动作未生效。
 * 清理/claim 释放失败通过 `AggregateError` 进入 `cause`。
 */
export function bindingFailure(
    requestId: string,
    code: string,
    error: unknown,
    cleanupConfirmed: boolean,
    cleanupError?: unknown,
): ChatActionError {
    const cause = withCleanupFailure(error, cleanupError);
    if (error instanceof ChatActionError) {
        const rebound = withRequestId(error, requestId) as ChatActionError;
        if (cleanupConfirmed || rebound.disposition !== "rejected")
            return rebound;
        return new ChatActionError(
            requestId,
            rebound.code,
            rebound.message,
            "unknown",
            { cause },
        );
    }
    return new ChatActionError(
        requestId,
        code,
        messageOf(error),
        cleanupConfirmed ? "rejected" : "unknown",
        { cause },
    );
}
