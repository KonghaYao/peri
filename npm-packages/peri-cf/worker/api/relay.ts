import type { Hono } from "hono";
import { HTTPException } from "hono/http-exception";
import type { Env } from "../types";

/**
 * 无状态、零配置的 BYOK 透传：浏览器宿主经同源端点访问存储与模型上游。
 *
 * - 上游由请求路径第一段携带（`/api/relay/<kind>/<encodeURIComponent(上游绝对基址)>/<上游路径>`），
 *   Worker 不持有任何地址或凭据；客户端凭据（authorization / x-api-key / api-key）原样带上游，
 *   逐跳头与跳源头仍被剥离。
 * - 存储响应里的 `base_url`（libSQL baton 亲和）改写回同一 relay 前缀并复用本请求的基址段，
 *   浏览器不直连上游。
 * - 模型响应可以是 SSE 流，必须流式透传，不得缓冲。
 * - 部署语义不变：拿 URL 即访问，入口控制由 Cloudflare Access 或等效设施承担。relay 现为
 *   按请求指定目标转发，未挂入口控制时会被当作开放转发面使用（已知风险，不做额外限制）。
 */

export const STORAGE_RELAY_PREFIX = "/api/relay/storage";
export const MODEL_RELAY_PREFIX = "/api/relay/model";

/** 逐跳头与跳源头：不向上游透传。客户端凭据（authorization 等）不在此列。 */
const REQUEST_HEADER_DENYLIST =
  /^(cookie|host|content-length|content-encoding|accept-encoding|connection|origin|referer|cf-|x-forwarded-)/i;
/** 上游响应中的逐跳与长度字段：由网关重新决定。 */
const RESPONSE_HEADER_DENYLIST =
  /^(content-encoding|content-length|transfer-encoding|connection|set-cookie|cf-)/i;

export type RelayKind = "storage" | "model";

interface RelayTarget {
  /** 转发目标：基址 origin + 基址 pathname（去尾斜杠）+ 请求余部路径；query 原样保留。 */
  url: URL;
  /** 请求路径第一段的原文（保持编码态）：改写 `base_url` 时复用，避免二次编码。 */
  encodedBase: string;
}

function invalidTarget(): HTTPException {
  return new HTTPException(400, {
    message: "relay target must be an absolute http(s) URL in the first path segment",
  });
}

function relayTarget(kind: RelayKind, request: Request): RelayTarget {
  const prefix = kind === "storage" ? STORAGE_RELAY_PREFIX : MODEL_RELAY_PREFIX;
  const incoming = new URL(request.url);
  const rest = incoming.pathname.slice(prefix.length);
  const separator = rest.indexOf("/", 1);
  const encodedBase = separator === -1 ? rest.slice(1) : rest.slice(1, separator);
  if (!encodedBase) throw invalidTarget();
  let base: URL;
  try {
    base = new URL(decodeURIComponent(encodedBase));
  } catch {
    throw invalidTarget();
  }
  if (base.protocol !== "http:" && base.protocol !== "https:") throw invalidTarget();
  const target = new URL(
    `${base.origin}${base.pathname.replace(/\/+$/, "")}${separator === -1 ? "" : rest.slice(separator)}`);
  target.search = incoming.search;
  return { url: target, encodedBase };
}

/** 把结构体内的绝对 `base_url` 改写为 relay 相对前缀；未命中时保持原字节。 */
function rewriteStorageBaseUrls(text: string, encodedBase: string): string {
  if (!text.includes('"base_url"')) return text;
  return text.split("\n").map((line) => {
    if (!line.includes('"base_url"')) return line;
    try {
      const value = JSON.parse(line) as unknown;
      if (!rewriteBaseUrlField(value, encodedBase)) return line;
      return JSON.stringify(value);
    } catch {
      return line;
    }
  }).join("\n");
}

function rewriteBaseUrlField(value: unknown, encodedBase: string): boolean {
  if (Array.isArray(value)) return value.map((entry) => rewriteBaseUrlField(entry, encodedBase)).some(Boolean);
  if (value === null || typeof value !== "object") return false;
  let rewritten = false;
  for (const [key, entry] of Object.entries(value)) {
    if (key === "base_url" && typeof entry === "string") {
      const replacement = relayRelative(entry, encodedBase);
      if (replacement !== undefined) {
        (value as Record<string, unknown>)[key] = replacement;
        rewritten = true;
      }
      continue;
    }
    if (rewriteBaseUrlField(entry, encodedBase)) rewritten = true;
  }
  return rewritten;
}

function relayRelative(candidate: string, encodedBase: string): string | undefined {
  let url: URL;
  try {
    url = new URL(candidate);
  } catch {
    return undefined;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return undefined;
  const path = url.pathname.replace(/\/+$/, "");
  return `${STORAGE_RELAY_PREFIX}/${encodedBase}${path}`;
}

export async function relayRequest(request: Request, kind: RelayKind): Promise<Response> {
  const target = relayTarget(kind, request);
  const headers = new Headers();
  for (const [name, value] of request.headers)
    if (!REQUEST_HEADER_DENYLIST.test(name)) headers.set(name, value);
  const method = request.method.toUpperCase();
  const response = await fetch(target.url, {
    method,
    headers,
    body: method === "GET" || method === "HEAD" ? undefined : request.body,
    redirect: "manual",
  });
  const responseHeaders = new Headers();
  for (const [name, value] of response.headers)
    if (!RESPONSE_HEADER_DENYLIST.test(name)) responseHeaders.set(name, value);
  const contentType = response.headers.get("content-type") ?? "";
  if (kind === "storage" && response.ok && contentType.includes("json")) {
    const rewritten = rewriteStorageBaseUrls(await response.text(), target.encodedBase);
    return new Response(rewritten, { status: response.status, statusText: response.statusText, headers: responseHeaders });
  }
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers: responseHeaders });
}

export function registerRelayRoutes(app: Hono<{ Bindings: Env }>): void {
  for (const [path, kind] of [
    ["/api/relay/storage", "storage"], ["/api/relay/storage/*", "storage"],
    ["/api/relay/model", "model"], ["/api/relay/model/*", "model"],
  ] as const) {
    app.all(path, (context) => relayRequest(context.req.raw, kind));
  }
}
