import type { Hono } from "hono";
import { HTTPException } from "hono/http-exception";
import type { Env } from "../types";

/**
 * 无状态、零配置的 BYOK 透传：浏览器宿主经同源端点访问存储、模型与 MCP 上游。
 *
 * - 上游由请求路径第一段携带（`/api/relay/<kind>/<encodeURIComponent(上游绝对基址)>/<上游路径>`），
 *   Worker 不持有任何地址或凭据；客户端凭据（authorization / x-api-key / api-key）原样带上游，
 *   逐跳头与跳源头仍被剥离。
 * - 存储响应里的 `base_url`（libSQL baton 亲和）改写回同一 relay 前缀并复用本请求的基址段，
 *   浏览器不直连上游。
 * - 模型响应可以是 SSE 流，必须流式透传，不得缓冲；MCP（streamable HTTP）的
 *   JSON / SSE 响应与 `mcp-session-id` 头同样原样透传。
 * - 部署语义不变：拿 URL 即访问，入口控制由 Cloudflare Access 或等效设施承担。relay 现为
 *   按请求指定目标转发，未挂入口控制时会被当作开放转发面使用（已知风险，不做额外限制）。
 */

export const STORAGE_RELAY_PREFIX = "/api/relay/storage";
export const MODEL_RELAY_PREFIX = "/api/relay/model";
export const MCP_RELAY_PREFIX = "/api/relay/mcp";

/** 逐跳头与跳源头：不向上游透传。客户端凭据（authorization 等）不在此列。 */
const REQUEST_HEADER_DENYLIST =
  /^(cookie|host|content-length|content-encoding|accept-encoding|connection|origin|referer|cf-|x-forwarded-)/i;
/** 上游响应中的逐跳与长度字段：由网关重新决定。 */
const RESPONSE_HEADER_DENYLIST =
  /^(content-encoding|content-length|transfer-encoding|connection|set-cookie|cf-)/i;

export type RelayKind = "storage" | "model" | "mcp";

const RELAY_PREFIXES: Record<RelayKind, string> = {
  storage: STORAGE_RELAY_PREFIX,
  model: MODEL_RELAY_PREFIX,
  mcp: MCP_RELAY_PREFIX,
};

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
  const prefix = RELAY_PREFIXES[kind];
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

/**
 * 原位把 `"base_url"` 的字符串值改写为 relay 相对前缀：只替换该 JSON 字符串字面量本身，
 * 文档内其它字节（数字字面量、空白与换行等）保持原样；单行 NDJSON 与美化多行 JSON 都生效。
 * 字符串内容里的同名字面量、`null` 与非字符串值、非绝对 http(s) URL 一律跳过。
 */
function rewriteStorageBaseUrls(text: string, encodedBase: string): string {
  if (!text.includes('"base_url"')) return text;
  let output = "";
  let cursor = 0;
  let index = 0;
  while (index < text.length) {
    if (text[index] !== '"') {
      index++;
      continue;
    }
    const keyEnd = stringLiteralEnd(text, index);
    if (keyEnd === -1) break;
    const colon = skipJsonWhitespace(text, keyEnd);
    if (text[colon] !== ":" || decodeStringLiteral(text.slice(index, keyEnd)) !== "base_url") {
      index = keyEnd;
      continue;
    }
    const valueStart = skipJsonWhitespace(text, colon + 1);
    if (text[valueStart] !== '"') {
      index = keyEnd;
      continue;
    }
    const valueEnd = stringLiteralEnd(text, valueStart);
    if (valueEnd === -1) break;
    const candidate = decodeStringLiteral(text.slice(valueStart, valueEnd));
    const replacement = candidate === undefined ? undefined : relayRelative(candidate, encodedBase);
    if (replacement !== undefined) {
      output += text.slice(cursor, valueStart) + JSON.stringify(replacement);
      cursor = valueEnd;
    }
    index = valueEnd;
  }
  return output + text.slice(cursor);
}

/** 返回字符串字面量结束后的下标；未闭合或非法转义未中止的返回 -1。 */
function stringLiteralEnd(text: string, start: number): number {
  for (let index = start + 1; index < text.length; index++) {
    const char = text[index];
    if (char === "\\") {
      index++;
      continue;
    }
    if (char === '"') return index + 1;
  }
  return -1;
}

function skipJsonWhitespace(text: string, start: number): number {
  let index = start;
  while (index < text.length) {
    const char = text[index];
    if (char !== " " && char !== "\t" && char !== "\n" && char !== "\r") break;
    index++;
  }
  return index;
}

/** 解析完整字符串字面量（含引号）；非法字面量返回 undefined，按不命中处理。 */
function decodeStringLiteral(literal: string): string | undefined {
  try {
    const value = JSON.parse(literal) as unknown;
    return typeof value === "string" ? value : undefined;
  } catch {
    return undefined;
  }
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
    ["/api/relay/mcp", "mcp"], ["/api/relay/mcp/*", "mcp"],
  ] as const) {
    app.all(path, (context) => relayRequest(context.req.raw, kind));
  }
}
