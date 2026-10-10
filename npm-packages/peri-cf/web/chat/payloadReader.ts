import {
  readPayload, PayloadReadError,
  type PayloadReadErrorCode, type ToolPayloadReadRef, type ToolPayloadReader,
} from '@peri-code/sdk/view';

/** 端点不可用时的占位 reader（未选择会话或未配置令牌）。 */
export const unavailablePayloadReader: ToolPayloadReader = async () => {
  throw new PayloadReadError('unavailable', '当前没有可用的全文读取端点。');
};

const KNOWN_CODES: PayloadReadErrorCode[] = ["unauthorized", "unavailable", "too_large", "integrity_error", "aborted", "transport_error"];

/**
 * 浏览器全文读取：同源 Bearer + no-store，经 /api/v2/chats/:id/payloads/:refId。
 * 服务端失败码沿用 SDK 契约；未知/格式不符的响应按 integrity_error 处理。
 */
export function httpPayloadReader(chatId: string, token: string, fetchImpl: typeof fetch = fetch): ToolPayloadReader {
  return async ({ id, signal }) => {
    const response = await fetchImpl(`/api/v2/chats/${encodeURIComponent(chatId)}/payloads/${encodeURIComponent(id)}`, {
      headers: { Authorization: `Bearer ${token}` }, cache: 'no-store', signal,
    });
    if (!response.ok) throw await payloadFailure(response);
    const payload = await response.json().catch(() => null) as { id?: unknown; bytes?: unknown; json?: unknown } | null;
    if (!payload || typeof payload.id !== 'string' || typeof payload.bytes !== 'number' || typeof payload.json !== 'string')
      throw new PayloadReadError('integrity_error', '载荷响应格式不正确。');
    return { id: payload.id, bytes: payload.bytes, json: payload.json };
  };
}

async function payloadFailure(response: Response): Promise<PayloadReadError> {
  if (response.status === 401 || response.status === 403) return new PayloadReadError('unauthorized', '身份验证失败，请在设置中检查访问令牌。');
  const body = await response.json().catch(() => null) as { error?: { code?: unknown; message?: unknown } } | null;
  const code = body?.error?.code;
  const message = typeof body?.error?.message === 'string' ? body.error.message : `载荷读取失败（HTTP ${response.status}）。`;
  if (typeof code === 'string' && (KNOWN_CODES as string[]).includes(code)) return new PayloadReadError(code as PayloadReadErrorCode, message);
  return new PayloadReadError('transport_error', message);
}

let current: ToolPayloadReader = unavailablePayloadReader;
export function setPayloadReader(reader: ToolPayloadReader): void { current = reader; }
export function getPayloadReader(): ToolPayloadReader { return current; }

export type PayloadTextResult = { ok: true; text: string } | { ok: false; error: string };

/** 校验与错误映射都在 SDK helper 内；这里只把结果收敛成可渲染的文本或原因。 */
export async function loadPayloadText(
  ref: ToolPayloadReadRef,
  reader: ToolPayloadReader = getPayloadReader(),
  signal?: AbortSignal,
): Promise<PayloadTextResult> {
  try {
    const response = await readPayload(ref, reader, { signal });
    return { ok: true, text: JSON.stringify(JSON.parse(response.json), null, 2) };
  } catch (error) {
    return { ok: false, error: error instanceof PayloadReadError ? error.message : '全文读取失败，请稍后重试。' };
  }
}
