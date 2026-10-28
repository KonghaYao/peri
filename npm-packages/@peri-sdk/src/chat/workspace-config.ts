import { WorkspaceMismatchError } from "../workspace/errors";
import { normalizeAbsolutePath, resolveWithin } from "../workspace/paths";
import type { WorkspaceRef } from "../workspace/types";

/** 一个 Chat 从 Workspace 派生出的同步配置。 */
export interface ResolvedChatWorkspace {
    /** 归一化后的 Workspace 副本；`runtimePath` 已规范化。 */
    readonly workspace: WorkspaceRef;
    /** Agent/session 根；`agent.cwd` 注入值。 */
    readonly runtimeRoot: string;
    /** 新会话 cwd；等于 runtimeRoot 或其子目录。 */
    readonly sessionCwd: string;
}

function isRecord(value: unknown): value is Record<string, unknown> {
    return value !== null && typeof value === "object" && !Array.isArray(value);
}

/**
 * 校验 `createChat({ workspace, sessionPath })` 并派生 cwd。
 *
 * - `runtimePath` 必须是绝对路径，归一化后作为 runtimeRoot；
 * - `sessionPath` 相对 runtimeRoot，为空表示根；绝对路径或 `..` 越界都以
 *   `workspace_mismatch` 拒绝；
 * - `hostPath` 可选（远程 sandbox 没有本地宿主路径），提供时必须是绝对路径。
 *
 * 路径比较是纯字符串判断，不解析符号链接；配置形状错误抛 `TypeError`。
 */
export function resolveChatWorkspace(
    workspace: unknown,
    sessionPath: unknown,
): ResolvedChatWorkspace {
    if (!isRecord(workspace)) {
        throw new TypeError(
            "createChat requires workspace: { id, runtimePath }",
        );
    }
    const { id, hostPath, runtimePath } = workspace;
    if (typeof id !== "string" || id.length === 0) {
        throw new TypeError("workspace.id must be a non-empty string");
    }
    const runtimeRoot = normalizeAbsolutePath(runtimePath);
    if (runtimeRoot === null) {
        throw new TypeError(
            "workspace.runtimePath must be an absolute path",
        );
    }

    let normalizedHostPath: string | undefined;
    if (hostPath !== undefined) {
        const normalized = normalizeAbsolutePath(hostPath);
        if (normalized === null) {
            throw new TypeError("workspace.hostPath must be an absolute path");
        }
        normalizedHostPath = normalized;
    }

    if (sessionPath !== undefined && typeof sessionPath !== "string") {
        throw new TypeError("sessionPath must be a relative path string");
    }
    const requestedSessionPath = sessionPath ?? "";
    const sessionCwd = resolveWithin(runtimeRoot, requestedSessionPath);
    if (sessionCwd === null) {
        throw new WorkspaceMismatchError(
            `Session path "${requestedSessionPath}" escapes the workspace runtime root "${runtimeRoot}"`,
        );
    }

    const resolved: WorkspaceRef = {
        id,
        ...(normalizedHostPath === undefined
            ? {}
            : { hostPath: normalizedHostPath }),
        runtimePath: runtimeRoot,
    };
    return {
        workspace: Object.freeze(resolved),
        runtimeRoot,
        sessionCwd,
    };
}
