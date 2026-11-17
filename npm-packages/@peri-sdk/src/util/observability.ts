/**
 * 观测快照的字符串安全投影；不依赖 Node/Bun/浏览器 API。
 *
 * 规则集中在这里，Chat/Sandbox/入口共用一份，避免各层各自决定哪些字段可输出。
 */

/** 稳定排序用比较；按 UTF-16 码元，不依赖 locale。 */
export function compareObservationIds(left: string, right: string): number {
    return left < right ? -1 : left > right ? 1 : 0;
}

/**
 * HTTP endpoint 的安全形式：origin + pathname。
 *
 * userinfo、query 与 hash 全部丢弃；只接受 http/https，非法值返回 undefined。
 */
export function safeEndpointUrl(
    value: string | null | undefined,
): string | undefined {
    if (typeof value !== "string" || value.length === 0) return undefined;
    let url: URL;
    try {
        url = new URL(value);
    } catch {
        return undefined;
    }
    if (url.protocol !== "http:" && url.protocol !== "https:") return undefined;
    return `${url.origin}${url.pathname}`;
}

const URL_IN_TEXT = /\b[a-z][a-z0-9+.-]*:\/\/[^\s"'<>`]+/gi;

/**
 * 敏感 header 名称；值取到行尾，Digest/Cookie 这类多段值也整体丢弃。
 *
 * `(?<![A-Za-z0-9])` 代替 `\b`，让 `client_secret` 这类下划线前缀也能命中。
 */
const SENSITIVE_HEADER =
    /(?<![A-Za-z0-9])(proxy-authorization|authorization|x-api-key|api-key|x-auth-token|auth-token|x-access-token|set-cookie|cookie|credential|credentials|client[_-]?secret|x-secret|password|x-password|secret)\b["']?\s*[:=][^\r\n]*/gi;

/** 独立的认证方案片段，例如 `Bearer <token>` / `Basic <base64>`。 */
const AUTH_SCHEME_TOKEN = /\b(Bearer|Basic)\s+[^\s"',;()[\]{}]+/gi;

/** 非 URL 的赋值形式；与 header 规则重叠的键由 header 规则收口。 */
const SENSITIVE_ASSIGNMENT =
    /(?<![A-Za-z0-9])(access[_-]?token|refresh[_-]?token|id[_-]?token|auth[_-]?token|api[_-]?key|apikey|client[_-]?secret|secret|password|token)\b["']?\s*[=:]\s*["']?[^\s&,;"'#]+/gi;

/**
 * 文本中的 URL 收敛为 origin + pathname；尾随标点留在原文里。
 *
 * http(s) 复用 endpoint 规则；其他 scheme（如 redis://）没有可用 origin，
 * 保留 scheme + host + pathname，同样丢弃 userinfo/query/fragment。非法 URL
 * 仍做保守裁剪，避免兜底路径泄露。
 */
function safeUrlInText(match: string): string {
    const trailing = /[)\].,;:!?]+$/.exec(match)?.[0] ?? "";
    const candidate =
        trailing.length === 0 ? match : match.slice(0, -trailing.length);
    let parsed: URL | undefined;
    try {
        parsed = new URL(candidate);
    } catch {
        parsed = undefined;
    }
    const safe =
        (parsed && safeEndpointUrl(candidate)) ??
        (parsed && parsed.host.length > 0
            ? `${parsed.protocol}//${parsed.host}${parsed.pathname}`
            : candidate
                  .replace(/#.*$/, "")
                  .replace(/\?.*$/, "")
                  .replace(
                      /^([a-z][a-z0-9+.-]*:\/\/)[^/@]*@/i,
                      "$1[redacted]@",
                  ));
    return `${safe}${trailing}`;
}

/**
 * 错误/文本脱敏的唯一权威，幂等。
 *
 * `SandboxHandle` 的错误映射、`SandboxManager.snapshot()` 与 `observeBackend()`
 * 的输出边界都走这里；handle 只额外按已知 token 做精确替换。
 */
export function redactObservationText(value: string): string {
    return value
        .replace(URL_IN_TEXT, safeUrlInText)
        .replace(AUTH_SCHEME_TOKEN, "$1 [redacted]")
        .replace(SENSITIVE_ASSIGNMENT, "$1=[redacted]")
        .replace(SENSITIVE_HEADER, "$1: [redacted]");
}

/** 只保留 header 名称并排序；空对象返回 undefined。 */
export function headerNamesOf(
    headers: Readonly<Record<string, string>> | undefined,
): string[] | undefined {
    if (headers === undefined) return undefined;
    const names = Object.keys(headers)
        .filter((name) => name.length > 0)
        .sort(compareObservationIds);
    return names.length === 0 ? undefined : names;
}
