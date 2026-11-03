/**
 * PeriInstance 的公开错误类型。
 *
 * 从实例模块拆出以保持 `peri-instance.ts` 在单文件规模上限内；实例模块继续
 * re-export，调用方的导入路径不变。
 */

export type PeriInstanceBindStage =
    | "mcp-connect" | "session/new" | "claim" | "session/load" | "rename" | "input-snapshot";

/** 绑定阶段失败；`createdSessionId` 已创建时上层不得盲目重复新建。 */
export class PeriInstanceBindError extends Error {
    constructor(
        readonly stage: PeriInstanceBindStage,
        readonly createdSessionId: string | null,
        message: string,
        options?: { cause?: unknown },
    ) {
        super(message, options);
        this.name = "PeriInstanceBindError";
    }
}

export class PeriInstanceNotBoundError extends Error {
    constructor(message = "Peri instance is not bound to a Session") {
        super(message);
        this.name = "PeriInstanceNotBoundError";
    }
}

/** 宿主清理未确认；资源保留，调用方可重试 close。 */
export class PeriHostCleanupError extends Error {
    constructor(message: string, options?: { cause?: unknown }) {
        super(message, options);
        this.name = "PeriHostCleanupError";
    }
}

/** 关闭未在预算内确认；资源保留，不产生 `closed` 事实，可第二次 close。 */
export class PeriInstanceCloseIncompleteError extends Error {
    readonly failures: readonly unknown[];

    constructor(
        failures: readonly unknown[] = [],
        message = "Peri instance close is incomplete; resources are preserved for retry",
        options?: { cause?: unknown },
    ) {
        super(message, {
            cause: options?.cause ?? (failures.length === 0
                ? undefined
                : failures.length === 1
                    ? failures[0]
                    : new AggregateError(failures, message)),
        });
        this.name = "PeriInstanceCloseIncompleteError";
        this.failures = failures;
    }
}
