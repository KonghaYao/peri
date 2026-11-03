import type { OpenUIActionEvent } from "@peri-code/sdk/openui";

/** 渲染层 action 事件由 `@peri-code/sdk/openui` 定义；聊天适配层再导出保持既有消费者不变。 */
export type { OpenUIActionEvent };

export type ParsedOpenUIAction =
  | { kind: "send"; text: string }
  | { kind: "open_url"; url: string }
  | { kind: "none" };

export type FormPrimitive = string | number | boolean;
export type CompactFormState = Record<string, FormPrimitive>;

const MAX_MESSAGE_CHARS = 8_000;
const MAX_FORM_STATE_CHARS = 2_000;
const MAX_URL_CHARS = 2_048;
const FORM_DATA_PREFIX = "\n\n表单数据：";
const FORM_TRUNCATED_NOTICE = "\n\n（表单数据过长，已截断）";
const MESSAGE_TRUNCATED_NOTICE = "\n\n（消息过长，已截断）";

function primitive(value: unknown): FormPrimitive | undefined {
  if (typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number") return Number.isFinite(value) ? value : undefined;
  return undefined;
}

function unwrapField(value: unknown): FormPrimitive | undefined {
  if (value !== null && typeof value === "object" && "value" in value) {
    return primitive((value as { value?: unknown }).value);
  }
  return primitive(value);
}

interface FormCompaction {
  readonly state: CompactFormState | undefined;
  readonly truncated: boolean;
}

function fitFormState(entries: Array<[string, FormPrimitive]>): { state: CompactFormState; truncated: boolean } {
  const result: CompactFormState = {};
  let truncated = false;
  for (const [key, value] of entries) {
    const candidate = { ...result, [key]: value };
    if (JSON.stringify(candidate).length <= MAX_FORM_STATE_CHARS) {
      result[key] = value;
      continue;
    }
    if (typeof value !== "string") {
      truncated = true;
      continue;
    }
    let shortened = value;
    while (shortened.length > 0) {
      shortened = shortened.slice(0, Math.floor(shortened.length / 2));
      if (JSON.stringify({ ...result, [key]: shortened }).length <= MAX_FORM_STATE_CHARS) {
        result[key] = shortened;
        break;
      }
    }
    truncated = true;
  }
  return { state: result, truncated };
}

function compactFormStateWithMeta(
  formState: Record<string, unknown> | undefined,
): FormCompaction {
  if (!formState) return { state: undefined, truncated: false };
  const entries: Array<[string, FormPrimitive]> = [];

  for (const [key, raw] of Object.entries(formState)) {
    if (key.startsWith("$") || key === "componentType") continue;
    const direct = unwrapField(raw);
    if (direct !== undefined) {
      entries.push([key, direct]);
      continue;
    }
    if (raw === null || typeof raw !== "object" || Array.isArray(raw)) continue;
    for (const [nestedKey, nestedRaw] of Object.entries(raw as Record<string, unknown>)) {
      if (nestedKey.startsWith("$") || nestedKey === "componentType") continue;
      const value = unwrapField(nestedRaw);
      if (value !== undefined) entries.push([`${key}.${nestedKey}`, value]);
    }
  }

  if (entries.length === 0) return { state: undefined, truncated: false };
  const { state, truncated } = fitFormState(entries);
  return {
    state: Object.keys(state).length > 0 ? state : undefined,
    truncated,
  };
}

/**
 * 表单状态只保留两层内的 primitive，去掉 `componentType` 等内部字段。
 * Renderer 的原始结构是 `{field: {value, componentType}}`（有 Form 时再包一层 formName）。
 */
export function compactFormState(formState: Record<string, unknown> | undefined): CompactFormState | undefined {
  return compactFormStateWithMeta(formState).state;
}

/** 只记录协议，避免把 userinfo、查询参数或超长 URL 原样写进日志。 */
function describeBlockedUrl(value: unknown): string {
  if (typeof value !== "string") return typeof value;
  const scheme = /^([a-z][a-z0-9+.-]*):/i.exec(value.trim())?.[1]?.toLowerCase();
  return scheme ? `${scheme}:…` : "(unparseable)";
}

function safeUrl(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const raw = value.trim();
  if (raw === "" || raw.length > MAX_URL_CHARS) return undefined;
  try {
    const url = new URL(raw);
    if (url.protocol !== "http:" && url.protocol !== "https:") return undefined;
    // 带 userinfo 的 URL 常用于伪装域名，直接拒绝。
    if (url.username !== "" || url.password !== "") return undefined;
    const normalized = url.toString();
    return normalized.length <= MAX_URL_CHARS ? normalized : undefined;
  } catch {
    return undefined;
  }
}

function messageOf(event: OpenUIActionEvent): string {
  const preferred = event.humanFriendlyMessage.trim();
  if (preferred) return preferred;
  const fallback = event.params.message;
  return typeof fallback === "string" ? fallback.trim() : "";
}

/**
 * 纯函数：把 Renderer action 转成宿主命令。
 * `continue_conversation` 附带精简表单值；空消息忽略。
 * `open_url` 只允许无 userinfo、长度 ≤2048 的 http/https。
 */
export function parseOpenUIAction(event: OpenUIActionEvent): ParsedOpenUIAction {
  if (event.type === "continue_conversation") {
    const rawMessage = messageOf(event);
    const message = rawMessage.slice(0, MAX_MESSAGE_CHARS);
    if (!message) return { kind: "none" };
    const messageText = rawMessage.length > MAX_MESSAGE_CHARS
      ? `${message}${MESSAGE_TRUNCATED_NOTICE}`
      : message;
    const form = compactFormStateWithMeta(event.formState);
    const formText = form.state
      ? `${FORM_DATA_PREFIX}${JSON.stringify(form.state)}${form.truncated ? FORM_TRUNCATED_NOTICE : ""}`
      : form.truncated
        ? FORM_TRUNCATED_NOTICE
        : "";
    return { kind: "send", text: `${messageText}${formText}` };
  }

  if (event.type === "open_url") {
    const url = safeUrl(event.params.url);
    if (!url) {
      console.warn("[openui] blocked action URL:", describeBlockedUrl(event.params.url));
      return { kind: "none" };
    }
    return { kind: "open_url", url };
  }

  console.warn("[openui] unsupported action type:", event.type);
  return { kind: "none" };
}
