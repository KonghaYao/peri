/**
 * Chat 动作错误契约（Phase 0 冻结）。
 *
 * `disposition` 表达调用方能否安全重试，与 HTTP 状态码是两件事：
 * - `rejected`：运行侧明确拒绝，动作未生效；可按错误类型决定是否修正后重试。
 * - `unknown`：动作可能已写出/执行，不得自动重试业务动作；先核对会话事实。
 * - `not_sent`：只由调用端在请求尚未提交时产生，不进入服务器 wire。
 *
 * Phase 5 的 CF 映射依据：`invalid_action` → 400；`busy`/`binding_conflict`/`not_ready`
 * → 409；`closed` → 503；`unknown`/内部错误 → 500。不得用 HTTP 状态反推 disposition。
 */
export type ChatDisposition = "rejected" | "unknown" | "not_sent";

export class ChatActionError extends Error {
    constructor(
        readonly requestId: string,
        readonly code: string,
        message: string,
        readonly disposition: ChatDisposition,
        options?: { cause?: unknown },
    ) {
        super(message, options);
        this.name = "ChatActionError";
    }
}

/** 形状/字段校验失败；HTTP/WS 边界与 SDK 内部均以 `invalid_action` 拒绝。 */
export class ChatActionValidationError extends ChatActionError {
    constructor(requestId: string, message: string, options?: { cause?: unknown }) {
        super(requestId, "invalid_action", message, "rejected", options);
        this.name = "ChatActionValidationError";
    }
}

/** 当前绑定仍有运行中执行、未结算输入/交互或绑定变更；不隐式取消或转移。 */
export class ChatBusyError extends ChatActionError {
    constructor(requestId: string, message: string, options?: { cause?: unknown }) {
        super(requestId, "busy", message, "rejected", options);
        this.name = "ChatBusyError";
    }
}

/** 句柄状态不允许该动作（例如非空句柄再次新建）；动作未生效。 */
export class ChatNotReadyError extends ChatActionError {
    constructor(requestId: string, message: string, options?: { cause?: unknown }) {
        super(requestId, "not_ready", message, "rejected", options);
        this.name = "ChatNotReadyError";
    }
}

/** 句柄已关闭或关闭未确认，不能再接收新动作。 */
export class ChatClosedError extends ChatActionError {
    constructor(requestId: string, message: string, options?: { cause?: unknown }) {
        super(requestId, "closed", message, "rejected", options);
        this.name = "ChatClosedError";
    }
}

/**
 * 同一 Session 已有控制者。
 * 本地冲突带 `ownerChatId`；跨 Manager 的 ownership 冲突不带。
 */
export class SessionBindingConflictError extends ChatActionError {
    constructor(requestId: string, message: string, readonly ownerChatId?: string) {
        super(requestId, "binding_conflict", message, "rejected");
        this.name = "SessionBindingConflictError";
    }
}

/**
 * Session 的 cwd 不在当前 Chat 的 workspace root 之下。
 *
 * 用于 `change_session` 的跨 workspace 护栏，也用于新会话 path 超出
 * `agent.cwd` 的配置错误；动作未生效，调用方可修正后重试。
 */
export class SessionWorkspaceMismatchError extends ChatActionError {
    constructor(
        requestId: string,
        readonly sessionPath: string,
        readonly workspaceRoot: string,
        options?: { cause?: unknown },
    ) {
        super(
            requestId,
            "workspace_mismatch",
            `Session path "${sessionPath}" is outside the chat workspace root "${workspaceRoot}"`,
            "rejected",
            options,
        );
        this.name = "SessionWorkspaceMismatchError";
    }
}

/**
 * `session/new` 已取得 sessionId，但后续阶段失败。
 * 清理确认后 SDK 会关闭已创建会话；清理未确认时保留资源与 ownership 供重试；
 * 调用方不得盲目重复新建。
 */
export class SessionCreationError extends ChatActionError {
    constructor(
        requestId: string,
        readonly createdSessionId: string,
        readonly stage: "session/new" | "rename" | "input-snapshot" | "claim",
        message: string,
        options?: { cause?: unknown },
    ) {
        super(requestId, "session_creation_failed", message, "unknown", options);
        this.name = "SessionCreationError";
    }
}

/**
 * `ChatService.close()` 未能确认全部资源已释放。
 *
 * 不是“已停止”的证明：ownership、索引与未确认的激活必须保留，供再次 close/shutdown 重试。
 * `failures` 保留每个失败的原始原因；`cause` 在多个失败时聚合为 `AggregateError`。
 */
export class ChatCloseIncompleteError extends Error {
    readonly failures: readonly unknown[];

    constructor(
        failures: readonly unknown[] = [],
        message = "Chat close is incomplete; resources and ownership are preserved for retry",
        options?: { cause?: unknown },
    ) {
        const chain = failures.length === 0
            ? undefined
            : failures.length === 1
                ? failures[0]
                : new AggregateError(failures, message);
        super(message, { cause: options?.cause ?? chain });
        this.name = "ChatCloseIncompleteError";
        this.failures = [...failures];
    }
}

/**
 * 事实日志已溢出，晚挂 View 无法拿到完整重放。
 * 必须显式要求重新绑定/等待下次 load，不能静默漏数据。
 */
export class ChatViewReplayOverflowError extends Error {
    constructor(message?: string, options?: { cause?: unknown }) {
        super(message, options);
        this.name = "ChatViewReplayOverflowError";
    }
}
