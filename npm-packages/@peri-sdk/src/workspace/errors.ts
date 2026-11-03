/** 稳定的 Workspace 配置错误码；Chat 动作层复用同一字符串。 */
export const WORKSPACE_MISMATCH = "workspace_mismatch" as const;

/**
 * Workspace 配置/路径不满足约束。
 *
 * 用于 `createChat` 的同步校验（`sessionPath` 越界、`agent.cwd` 与
 * `runtimePath` 不一致）。Chat 动作期间的同类拒绝使用
 * `SessionWorkspaceMismatchError`，两者共享 `workspace_mismatch` 错误码。
 */
export class WorkspaceMismatchError extends Error {
    readonly code = WORKSPACE_MISMATCH;

    constructor(message: string, options?: { cause?: unknown }) {
        super(message, options);
        this.name = "WorkspaceMismatchError";
    }
}
