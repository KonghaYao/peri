/**
 * Workspace 路径工具（纯 JS，不引入 `node:path`/`node:os`）。
 *
 * 只做字符串层面的归一化与范围判断，不解析符号链接、不访问文件系统。
 * 接受 `/` 与 `\` 分隔符、`.`/`..` 段与 Windows 盘符。
 */

function drivePrefixOf(path: string): string {
    const drive = /^([A-Za-z]):(?:\/|$)/u.exec(path);
    return drive ? `${drive[1]!.toLowerCase()}:` : "";
}

/**
 * 归一化路径：统一分隔符、解析 `.`/`..`、小写盘符。
 *
 * 绝对路径的 `..` 不会越过根；相对路径保留开头无法消解的 `..`。
 * 空输入返回 `null`。
 */
export function normalizePath(value: unknown): string | null {
    if (typeof value !== "string") return null;
    let path = value.trim().replace(/\\/gu, "/");
    if (path.length === 0) return null;
    const drivePrefix = drivePrefixOf(path);
    if (drivePrefix !== "") path = path.slice(2);
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

/** 归一化后的路径是否为绝对路径（`/...` 或 `x:/...`）。 */
export function isAbsolutePath(value: string): boolean {
    return value.startsWith("/") || /^[a-z]:\//u.test(value);
}

/** 文件系统/盘符根（`/` 或 `x:/`）；其下任意绝对路径都属于该根。 */
function isRootPath(value: string): boolean {
    return value === "/" || /^[a-z]:\/$/u.test(value);
}

/**
 * 归一化绝对路径；相对路径、非字符串或空串返回 `null`。
 *
 * `WorkspaceRef.runtimePath` 必须通过本函数。
 */
export function normalizeAbsolutePath(value: unknown): string | null {
    const normalized = normalizePath(value);
    if (normalized === null || !isAbsolutePath(normalized)) return null;
    return normalized;
}

/**
 * 把相对路径解析到绝对 `root` 内。
 *
 * - `relative` 为空时返回归一化后的 root；
 * - 绝对 `relative`、或 `..` 使结果越出 root 时返回 `null`；
 * - 不做符号链接解析。
 */
export function resolveWithin(root: unknown, relative: unknown): string | null {
    const normalizedRoot = normalizeAbsolutePath(root);
    if (normalizedRoot === null) return null;
    if (relative === undefined || relative === null || relative === "") {
        return normalizedRoot;
    }
    if (typeof relative !== "string") return null;
    const raw = relative.trim().replace(/\\/gu, "/");
    if (raw.length === 0) return normalizedRoot;
    // 相对路径若带盘符/前导斜杠，说明调用方误传了绝对路径。
    if (isAbsolutePath(raw) || drivePrefixOf(raw) !== "") return null;

    const joined = isRootPath(normalizedRoot)
        ? `${normalizedRoot}${raw}`
        : `${normalizedRoot}/${raw}`;
    const resolved = normalizeAbsolutePath(joined);
    if (resolved === null) return null;
    if (isRootPath(normalizedRoot)) return resolved;
    if (resolved === normalizedRoot) return resolved;
    return resolved.startsWith(`${normalizedRoot}/`) ? resolved : null;
}
