import {
  ChatActionError,
  ChatActionValidationError,
  ChatBusyError,
  ChatClosedError,
  ChatNotReadyError,
  PayloadReadError,
  SessionBindingConflictError,
  SessionCreationError,
  type ChatDisposition,
} from "@peri-code/sdk";

/** HTTP 边界只负责把领域错误映射为状态与稳定 code；disposition 不反推。 */
export interface ChatErrorBody {
  ok: false;
  code: string;
  message: string;
  disposition: ChatDisposition;
  requestId?: string;
  chatId?: string;
  createdSessionId?: string;
  stage?: string;
}

export interface ChatErrorContext {
  chatId?: string;
  requestId?: string;
}

export interface MappedError {
  status: number;
  body: ChatErrorBody;
}

function statusFor(error: ChatActionError): number {
  if (error instanceof ChatActionValidationError) return 400;
  if (
    error instanceof ChatBusyError
    || error instanceof ChatNotReadyError
    || error instanceof SessionBindingConflictError
  ) return 409;
  if (error instanceof ChatClosedError) return 503;
  // session/new 已取得身份但后续失败：调用方必须先核对，不能盲目重建。
  if (error instanceof SessionCreationError) return 500;
  switch (error.code) {
    case "invalid_action": return 400;
    case "busy":
    case "binding_conflict":
    case "not_ready": return 409;
    case "closed": return 503;
    default: return 500;
  }
}

/**
 * Action 错误映射。
 *
 * `invalid_action` → 400；busy/binding_conflict/not_ready → 409；closed → 503；
 * 绑定失败、session 创建失败与 unknown/internal → 500。未知错误统一 unknown，
 * 绝不声称动作未生效。
 */
export function mapChatActionError(
  error: unknown,
  context: ChatErrorContext = {},
): MappedError {
  if (error instanceof ChatActionError) {
    const requestId = error.requestId || context.requestId;
    const body: ChatErrorBody = {
      ok: false,
      code: error.code,
      message: error.message,
      disposition: error.disposition,
      ...(requestId ? { requestId } : {}),
      ...(context.chatId ? { chatId: context.chatId } : {}),
    };
    if (error instanceof SessionCreationError) {
      body.createdSessionId = error.createdSessionId;
      body.stage = error.stage;
    }
    return { status: statusFor(error), body };
  }
  return {
    status: 500,
    body: {
      ok: false,
      code: "internal_error",
      message: "命令执行失败；诊断已记录",
      disposition: "unknown",
      ...(context.requestId ? { requestId: context.requestId } : {}),
      ...(context.chatId ? { chatId: context.chatId } : {}),
    },
  };
}

/** 未知 Chat：句柄已关闭或从未登记。 */
export function chatNotFound(chatId: string): MappedError {
  return {
    status: 410,
    body: {
      ok: false,
      code: "chat_not_found",
      message: "Chat not found",
      disposition: "rejected",
      chatId,
    },
  };
}

/** 原因链有界展开；不序列化任意对象，避免把请求/凭据带进响应。 */
function describeFailure(error: unknown, depth = 0): string {
  if (!(error instanceof Error)) return String(error);
  const cause = depth < 3 && error.cause !== undefined
    ? ` <- ${describeFailure(error.cause, depth + 1)}`
    : "";
  return `${error.name}: ${error.message}${cause}`;
}

export interface CloseIncompleteBody {
  ok: false;
  code: "close_incomplete";
  message: string;
  disposition: "unknown";
  incomplete: true;
  errors: string[];
  chatId?: string;
}

/** close 未确认：不是“已停止”的证明，保留资源与原因供重试。 */
export function closeIncomplete(
  error: Error & { failures?: readonly unknown[] },
  chatId: string,
): { status: 500; body: CloseIncompleteBody } {
  return {
    status: 500,
    body: {
      ok: false,
      code: "close_incomplete",
      message: error.message,
      disposition: "unknown",
      incomplete: true,
      errors: (error.failures ?? []).map((failure) => describeFailure(failure)),
      chatId,
    },
  };
}

const PAYLOAD_STATUS: Record<PayloadReadError["code"], number> = {
  unauthorized: 403,
  unavailable: 404,
  too_large: 413,
  aborted: 408,
  integrity_error: 502,
  transport_error: 502,
};

export function mapPayloadError(error: unknown): {
  status: number;
  body: { ok: false; code: string; message: string };
} {
  if (error instanceof PayloadReadError) {
    return {
      status: PAYLOAD_STATUS[error.code],
      body: { ok: false, code: error.code, message: error.message },
    };
  }
  return {
    status: 500,
    body: {
      ok: false,
      code: "internal_error",
      message: "载荷读取失败；诊断已记录",
    },
  };
}
