import { RpcError } from "../transport/rpc-error";
import {
    ChatActionError,
    ChatBusyError,
    ChatClosedError,
    ChatNotReadyError,
    SessionBindingConflictError,
    SessionCreationError,
    SessionWorkspaceMismatchError,
    type ChatDisposition,
} from "./errors";

export function messageOf(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
}

/** JSON-RPC 内部错误：运行侧可能已产生副作用，不能当作明确拒绝。 */
const JSON_RPC_INTERNAL_ERROR = -32603;

/**
 * 控制类动作失败的可重试性。
 *
 * JSON-RPC 错误响应是运行侧的明确拒绝（动作未生效）；传输失败与
 * -32603 内部错误的结果未知，不能自动重放。
 */
export function controlFailureDisposition(error: unknown): ChatDisposition {
    return error instanceof RpcError && error.code !== JSON_RPC_INTERNAL_ERROR
        ? "rejected"
        : "unknown";
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
    if (error instanceof SessionWorkspaceMismatchError) {
        return new SessionWorkspaceMismatchError(
            requestId,
            error.sessionPath,
            error.workspaceRoot,
            { cause: error },
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
 * 清理/ownership 释放失败通过 `AggregateError` 进入 `cause`。
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
