import {
  parseChatUserAction,
  type ChatBindingTarget,
  type ChatUserAction,
  type ReadPayloadOptions,
  type ToolPayloadReadRef,
  type ToolPayloadResponse,
} from "@peri-code/sdk";
import { PayloadReadError } from "@peri-code/sdk/view";
import {
  actionTargetSchema, chatActionSuccessSchema, chatErrorResponseSchema, chatHandleSchema,
  payloadErrorResponseSchema, payloadResponseSchema,
  type ActionTargetDTO, type ChatActionSuccessDTO, type ChatHandleDTO,
} from "./contracts";

/** Action 失败：`disposition` 是唯一可重试性权威，HTTP 状态不反推。 */
export type RemoteDisposition = "rejected" | "unknown" | "not_sent";

export class RemoteActionError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly disposition: RemoteDisposition,
    readonly options: {
      status?: number;
      requestId?: string;
      chatId?: string;
      createdSessionId?: string;
      stage?: string;
      cause?: unknown;
    } = {},
  ) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "RemoteActionError";
  }

  get status(): number | undefined { return this.options.status; }
  get requestId(): string | undefined { return this.options.requestId; }
  get chatId(): string | undefined { return this.options.chatId; }
  get createdSessionId(): string | undefined { return this.options.createdSessionId; }
  get stage(): string | undefined { return this.options.stage; }
}

/** `POST /api/chats` 的句柄；打开历史会话时先取得它，再发 change_session。 */
export interface RemoteChatHandle {
  id: string;
  sessionId: string | null;
  bindingRevision: number;
}

export interface RemoteActionClientOptions {
  origin?: string;
  fetch?: typeof fetch;
}

function apiUrl(base: string, path: string): string {
  return new URL(path, base || globalThis.location.origin).href;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

async function readJson(response: Response): Promise<unknown> {
  return await response.json().catch(() => undefined);
}

function failureFromResponse(status: number, body: unknown): RemoteActionError {
  const parsed = chatErrorResponseSchema.safeParse(body);
  if (!parsed.success) {
    return new RemoteActionError("internal_error", `请求失败（HTTP ${status}）`, "unknown", { status });
  }
  const value = parsed.data;
  return new RemoteActionError(value.code, value.message, value.disposition, {
    status,
    ...(value.requestId ? { requestId: value.requestId } : {}),
    ...(value.chatId ? { chatId: value.chatId } : {}),
    ...(value.createdSessionId ? { createdSessionId: value.createdSessionId } : {}),
    ...(value.stage ? { stage: value.stage } : {}),
  });
}

function transportFailure(error: unknown, message = "网络请求失败，动作执行状态未知。"): RemoteActionError {
  return new RemoteActionError("transport_error", message, "unknown", { cause: error });
}

/** 客户端预检/本地校验失败：请求未提交，可安全修正后重试。 */
function notSentError(code: string, message: string): RemoteActionError {
  return new RemoteActionError(code, message, "not_sent");
}

/** 目标绑定必须是纯数据；空句柄固定 `{sessionId:null,bindingRevision:0}`。 */
export function parseActionTarget(raw: unknown): ActionTargetDTO {
  const parsed = actionTargetSchema.safeParse(raw);
  if (!parsed.success) throw notSentError("invalid_target", "动作目标绑定格式不合法，命令未发送。");
  return parsed.data;
}

export interface ActionReceipt {
  requestId: string;
  chatId: string;
  sessionId: string;
  bindingRevision: number;
  inputId?: string;
  accepted?: boolean;
  status?: string;
  applied?: number;
  nextCursor?: string | null;
  raw: ChatActionSuccessDTO;
}

/** 发送回执：HTTP 202 只证明入队被接受；`isSent`/`then` 由副本队列事实结算。 */
export class RemoteSendReceipt implements PromiseLike<void> {
  private sent = false;
  private readonly delivery: Promise<void>;

  constructor(
    readonly requestId: string,
    readonly inputId: string,
    readonly target: ChatBindingTarget,
    readonly accepted: Promise<void>,
    delivery: Promise<void>,
  ) {
    this.delivery = delivery.then(
      () => { this.sent = true; },
      (error: unknown) => { throw error; },
    );
    // 未观察的投递等待不得升级为全局未处理拒绝。
    void this.delivery.catch(() => {});
  }

  get isSent(): boolean { return this.sent; }

  then<TResult1 = void, TResult2 = never>(
    onfulfilled?: ((value: void) => TResult1 | PromiseLike<TResult1>) | null,
    onrejected?: ((reason: unknown) => TResult2 | PromiseLike<TResult2>) | null,
  ): Promise<TResult1 | TResult2> {
    return this.delivery.then(onfulfilled, onrejected);
  }
}

function parseSuccess(response: Response, body: unknown, action: ChatUserAction): ActionReceipt {
  const parsed = chatActionSuccessSchema.safeParse(body);
  if (response.status !== 202 || !parsed.success) {
    throw new RemoteActionError("transport_error", `动作回执不可解析（HTTP ${response.status}）。`, "unknown", {
      status: response.status,
      requestId: action.requestId,
    });
  }
  const value = parsed.data;
  if (value.requestId !== action.requestId) {
    throw new RemoteActionError("transport_error", "动作回执的 requestId 不匹配。", "unknown", {
      status: response.status,
      requestId: action.requestId,
    });
  }
  return {
    requestId: value.requestId,
    chatId: value.chatId,
    sessionId: value.sessionId,
    bindingRevision: value.bindingRevision,
    ...(value.inputId === undefined ? {} : { inputId: value.inputId }),
    ...(value.accepted === undefined ? {} : { accepted: value.accepted }),
    ...(value.status === undefined ? {} : { status: value.status }),
    ...(value.applied === undefined ? {} : { applied: value.applied }),
    ...(value.nextCursor === undefined ? {} : { nextCursor: value.nextCursor }),
    raw: value,
  };
}

/**
 * 新 HTTP 面：句柄创建、Action、载荷与显式关闭。
 *
 * 不接触旧业务门面、旧命令/事件路由或目录运行态。
 */
export function createRemoteActions(options: RemoteActionClientOptions = {}) {
  const base = options.origin ?? "";
  const fetchImpl = options.fetch ?? globalThis.fetch.bind(globalThis);

  async function createHandle(path?: string, signal?: AbortSignal): Promise<RemoteChatHandle> {
    let response: Response;
    try {
      response = await fetchImpl(apiUrl(base, "/api/chats"), {
        method: "POST",
        headers: { "content-type": "application/json", accept: "application/json" },
        body: JSON.stringify(path === undefined ? {} : { path }),
        signal,
      });
    } catch (error) {
      throw transportFailure(error, "创建会话句柄失败，结果未知。");
    }
    const body = await readJson(response);
    if (!response.ok) throw failureFromResponse(response.status, isRecord(body) ? body : undefined);
    const parsed = isRecord(body)
      ? chatHandleSchema.safeParse(body.chat)
      : { success: false as const };
    if (!parsed.success)
      throw new RemoteActionError("transport_error", "句柄回执不可解析。", "unknown", { status: response.status });
    const handle: ChatHandleDTO = parsed.data;
    return handle;
  }

  async function execute(
    chatAddress: string,
    action: ChatUserAction,
    target: ActionTargetDTO,
    signal?: AbortSignal,
  ): Promise<ActionReceipt> {
    let parsedAction: ChatUserAction;
    try {
      parsedAction = parseChatUserAction(action);
    } catch (error) {
      throw notSentError("invalid_action", error instanceof Error ? error.message : "动作格式不合法。");
    }
    const parsedTarget = parseActionTarget(target);
    let response: Response;
    try {
      response = await fetchImpl(apiUrl(base, `/api/chats/${encodeURIComponent(chatAddress)}/actions`), {
        method: "POST",
        headers: { "content-type": "application/json", accept: "application/json" },
        body: JSON.stringify({ ...parsedAction, target: parsedTarget }),
        signal,
      });
    } catch (error) {
      throw transportFailure(error);
    }
    const body = await readJson(response);
    if (!response.ok) throw failureFromResponse(response.status, body);
    return parseSuccess(response, body, parsedAction);
  }

  async function close(chatAddress: string): Promise<void> {
    let response: Response;
    try {
      response = await fetchImpl(apiUrl(base, `/api/chats/${encodeURIComponent(chatAddress)}`), {
        method: "DELETE",
        headers: { accept: "application/json" },
      });
    } catch (error) {
      throw transportFailure(error, "关闭会话句柄失败，结果未知。");
    }
    if (response.status === 204) return;
    const body = await readJson(response);
    if (response.status === 410) {
      // 句柄已不存在：关闭目标已经达成。
      return;
    }
    throw failureFromResponse(response.status, body);
  }

  async function readPayload(
    chatAddress: string,
    ref: ToolPayloadReadRef,
    options: ReadPayloadOptions = {},
  ): Promise<ToolPayloadResponse> {
    const query = `?bytes=${encodeURIComponent(String(ref.bytes))}`;
    let response: Response;
    try {
      response = await fetchImpl(
        apiUrl(base, `/api/chats/${encodeURIComponent(chatAddress)}/payload/${encodeURIComponent(ref.id)}${query}`),
        { headers: { accept: "application/json" }, signal: options.signal },
      );
    } catch (error) {
      if (options.signal?.aborted) throw new PayloadReadError("aborted", "全文读取已取消。");
      throw new PayloadReadError("transport_error", error instanceof Error ? error.message : "全文读取失败。");
    }
    const body = await readJson(response);
    if (response.ok) {
      const parsed = payloadResponseSchema.safeParse(body);
      if (!parsed.success) throw new PayloadReadError("integrity_error", "载荷响应不可解析。");
      return parsed.data.payload;
    }
    const parsed = payloadErrorResponseSchema.safeParse(body);
    if (!parsed.success) throw new PayloadReadError("transport_error", `全文读取失败（HTTP ${response.status}）。`);
    const code = parsed.data.code;
    const known = ["unauthorized", "unavailable", "too_large", "integrity_error", "aborted", "transport_error"];
    if (!known.includes(code))
      throw new PayloadReadError("transport_error", `${parsed.data.message}（${code}）`);
    throw new PayloadReadError(code as PayloadReadError["code"], parsed.data.message);
  }

  return { createHandle, execute, close, readPayload };
}

export type RemoteActions = ReturnType<typeof createRemoteActions>;
