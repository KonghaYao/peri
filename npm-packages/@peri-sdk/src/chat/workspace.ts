/**
 * Chat 工作区路径判断（纯 JS，不引入 `node:path`）。
 *
 * 只做“同一路径或后代”判定，不解析符号链接、不访问文件系统。接受 `/` 与
 * `\` 分隔符、`.`/`..` 段与 Windows 盘符；无法比较（相对路径、非字符串）
 * 时返回 `unknown`，由调用方跳过校验而不是猜测。
 */
export type WorkspaceRelation = "within" | "outside" | "unknown";

function normalizePath(value: unknown): string | null {
    if (typeof value !== "string") return null;
    let path = value.trim().replace(/\\/gu, "/");
    if (path.length === 0) return null;
    const drive = /^([A-Za-z]):(?:\/|$)/u.exec(path);
    const drivePrefix = drive ? `${drive[1]!.toLowerCase()}:` : "";
    if (drive) path = path.slice(2);
    const absolute = path.startsWith("/");
    const segments: string[] = [];
    for (const segment of path.split("/")) {
        if (segment.length === 0 || segment === ".") continue;
        if (segment === "..") {
            if (segments.length > 0 && segments[segments.length - 1] !== "..") {
                segments.pop();
            } else if (!absolute && drivePrefix === "") {
                segments.push("..");
            }
            continue;
        }
        segments.push(segment);
    }
    const joined = segments.join("/");
    if (drivePrefix !== "") return `${drivePrefix}/${joined}`;
    if (absolute) return `/${joined}`;
    return joined.length === 0 ? "." : joined;
}

function isAbsoluteNormalized(path: string): boolean {
    return path.startsWith("/") || /^[a-z]:\//u.test(path);
}

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
        !isAbsoluteNormalized(normalizedRoot) ||
        !isAbsoluteNormalized(normalizedCandidate)
    ) {
        return "unknown";
    }
    if (normalizedRoot === "/") return "within";
    if (normalizedCandidate === normalizedRoot) return "within";
    return normalizedCandidate.startsWith(`${normalizedRoot}/`)
        ? "within"
        : "outside";
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
