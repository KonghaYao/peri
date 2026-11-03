import { isAbsolutePath, normalizePath } from "../workspace/paths";

/**
 * Chat 工作区路径判断（纯 JS，不引入 `node:path`）。
 *
 * 只做“同一路径或后代”判定，不解析符号链接、不访问文件系统。归一化规则见
 * `workspace/paths`；无法比较（相对路径、非字符串）时返回 `unknown`，
 * 由调用方决定拒绝还是跳过。
 */
export type WorkspaceRelation = "within" | "outside" | "unknown";

/**
 * 判断 `candidate` 是否等于 `root` 或位于其下。
 *
 * - root 缺失/为空：`unknown`（调用方跳过校验）；
 * - 任一路径为相对路径：`unknown`（无法仅凭字符串确定基准）；
 * - 统一分隔符、解析 `.`/`..` 后做段边界比较，`/workspace-other` 不算
 *   `/workspace` 的后代。
 */
export function workspaceRelation(
    root: string | undefined,
    candidate: string | undefined,
): WorkspaceRelation {
    const normalizedRoot = normalizePath(root);
    const normalizedCandidate = normalizePath(candidate);
    if (normalizedRoot === null || normalizedCandidate === null) {
        return "unknown";
    }
    if (
        !isAbsolutePath(normalizedRoot) ||
        !isAbsolutePath(normalizedCandidate)
    ) {
        return "unknown";
    }
    if (normalizedRoot === "/") return "within";
    if (normalizedCandidate === normalizedRoot) return "within";
    if (normalizedCandidate.startsWith(`${normalizedRoot}/`)) return "within";
    // Windows 盘符根（`c:/`）的尾斜杠不能用于拼接后代前缀。
    if (/^[a-z]:\/$/u.test(normalizedRoot)) {
        return normalizedCandidate.startsWith(normalizedRoot)
            ? "within"
            : "outside";
    }
    return "outside";
}

/** 读取 `agentConfig.cwd`；非字符串/空串视为未配置。 */
export function agentConfigCwd(agentConfig: unknown): string | undefined {
    if (
        agentConfig === null ||
        typeof agentConfig !== "object" ||
        Array.isArray(agentConfig)
    ) {
        return undefined;
    }
    const cwd = (agentConfig as Record<string, unknown>).cwd;
    return typeof cwd === "string" && cwd.length > 0 ? cwd : undefined;
}
