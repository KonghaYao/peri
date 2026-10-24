import {
  readPayload, PayloadReadError,
  type PayloadReadErrorCode, type ToolPayloadReadRef, type ToolPayloadReader,
} from '@peri-code/sdk/view';
import type { CommandFailure, CommandOutcome } from '../../remote';

/** 端点不可用时的占位 reader（未选择会话或宿主尚未就绪）。 */
export const unavailablePayloadReader: ToolPayloadReader = async () => {
  throw new PayloadReadError('unavailable', '当前没有可用的全文读取端点。');
};

const KNOWN_CODES: PayloadReadErrorCode[] = ["unauthorized", "unavailable", "too_large", "integrity_error", "aborted", "transport_error"];

/** 全文读取只依赖「会话命令通道」这一窄接口，由 AgentManager 实现；不直接触碰宿主。 */
export interface PayloadCommandSource {
  readPayload(refId: string, signal?: AbortSignal): Promise<CommandOutcome>;
}

/**
 * 浏览器全文读取：经当前会话命令通道下发 `{op:"payload", refId}`；未就绪时报告 unavailable。
 */
export function commandPayloadReader(source: () => PayloadCommandSource | undefined): ToolPayloadReader {
  return async ({ id, signal }) => {
    const channel = source();
    if (!channel) throw new PayloadReadError('unavailable', '会话尚未就绪，无法读取全文。');
    const result = await channel.readPayload(id, signal);
    if (!result.ok) throw payloadFailure(result);
    if (!result.payload) throw new PayloadReadError('integrity_error', '载荷响应缺少正文。');
    return { id: result.payload.id, bytes: result.payload.bytes, json: result.payload.json };
  };
}

/** 处置优先于失败码：not_sent 表示未提交，unknown 表示结果未知；明确拒绝再沿用 SDK 契约。 */
function payloadFailure(failure: CommandFailure): PayloadReadError {
  const message = failure.message || '载荷读取失败。';
  if (failure.disposition === 'not_sent') return new PayloadReadError('unavailable', message);
  if (failure.disposition === 'unknown') return new PayloadReadError('transport_error', message);
  if ((KNOWN_CODES as string[]).includes(failure.code)) return new PayloadReadError(failure.code as PayloadReadErrorCode, message);
  return new PayloadReadError('integrity_error', `载荷读取被拒绝（${failure.code}）：${message}`);
}

let current: ToolPayloadReader = unavailablePayloadReader;
export function setPayloadReader(reader: ToolPayloadReader): void { current = reader; }
export function getPayloadReader(): ToolPayloadReader { return current; }

export type PayloadTextResult = { ok: true; text: string } | { ok: false; error: string };
export type PayloadValueResult = { ok: true; value: unknown } | { ok: false; error: string };

/** 读取并校验全文，保留原始 JSON 值供展示层按领域语义格式化。 */
export async function loadPayloadValue(
  ref: ToolPayloadReadRef,
  reader: ToolPayloadReader = getPayloadReader(),
  signal?: AbortSignal,
): Promise<PayloadValueResult> {
  try {
    const response = await readPayload(ref, reader, { signal });
    try { return { ok: true, value: JSON.parse(response.json) }; }
    catch { return { ok: false, error: '全文内容不是合法 JSON。' }; }
  } catch (error) {
    return { ok: false, error: error instanceof PayloadReadError ? error.message : '全文读取失败，请稍后重试。' };
  }
}

/** 校验与错误映射都在 SDK helper 内；这里只把结果收敛成可渲染的文本或原因。 */
export async function loadPayloadText(
  ref: ToolPayloadReadRef,
  reader: ToolPayloadReader = getPayloadReader(),
  signal?: AbortSignal,
): Promise<PayloadTextResult> {
  const result = await loadPayloadValue(ref, reader, signal);
  if (!result.ok) return result;
  return { ok: true, text: JSON.stringify(result.value, null, 2) };
}
