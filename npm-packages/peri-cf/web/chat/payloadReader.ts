import {
  readPayload, PayloadReadError,
  type PayloadReadErrorCode, type ToolPayloadReadRef, type ToolPayloadReader,
} from '@peri-code/sdk/view';
import type { CommandFailure, CommandOutcome } from '../api/sync';

/** 端点不可用时的占位 reader（未选择会话或未配置令牌）。 */
export const unavailablePayloadReader: ToolPayloadReader = async () => {
  throw new PayloadReadError('unavailable', '当前没有可用的全文读取端点。');
};

const KNOWN_CODES: PayloadReadErrorCode[] = ["unauthorized", "unavailable", "too_large", "integrity_error", "aborted", "transport_error"];

/** 全文读取只依赖「会话命令通道」这一窄接口，由当前 ChatSession 实现；不直接触碰 socket。 */
export interface PayloadCommandSource {
  readPayload(refId: string, signal?: AbortSignal): Promise<CommandOutcome>;
}

/**
 * 浏览器全文读取：经当前会话那条已鉴权的 WS 连接下发 `{op:"payload", refId}`。
 * 载荷驻留在会话执行上下文里，HTTP 没有会话亲和性；未就绪时明确报告 unavailable，不伪造内容。
 */
export function commandPayloadReader(source: () => PayloadCommandSource | undefined): ToolPayloadReader {
  return async ({ id, signal }) => {
    const channel = source();
    if (!channel) throw new PayloadReadError('unavailable', '会话同步尚未就绪，无法读取全文。');
    const result = await channel.readPayload(id, signal);
    if (!result.ok) throw payloadFailure(result);
    if (!result.payload) throw new PayloadReadError('integrity_error', '载荷响应缺少正文。');
    return { id: result.payload.id, bytes: result.payload.bytes, json: result.payload.json };
  };
}

/** 服务端失败码沿用 SDK 契约；能解析但语义未知的拒绝按契约不符处理，不伪装成传输失败。 */
function payloadFailure(failure: CommandFailure): PayloadReadError {
  const message = failure.message || '载荷读取失败。';
  if ((KNOWN_CODES as string[]).includes(failure.code)) return new PayloadReadError(failure.code as PayloadReadErrorCode, message);
  return new PayloadReadError('integrity_error', `载荷读取被拒绝（${failure.code}）：${message}`);
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
