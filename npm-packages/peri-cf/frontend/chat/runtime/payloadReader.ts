import {
  readPayload, PayloadReadError,
  type ToolPayloadReadRef, type ToolPayloadReader, type ToolPayloadResponse,
} from '@peri-code/sdk/view';

/** 端点不可用时的占位 reader（未选择会话或同步尚未就绪）。 */
const unavailablePayloadReader: ToolPayloadReader = async () => {
  throw new PayloadReadError('unavailable', '当前没有可用的全文读取端点。');
};

/** 全文读取只依赖控制器的窄接口；字节数由副本里的引用事实决定。 */
export interface PayloadSource {
  readPayloadById(id: string, signal?: AbortSignal): Promise<ToolPayloadResponse>;
}

/**
 * 浏览器全文读取：经鉴权 HTTP 端点读取；未选择会话时报告 unavailable，
 * 引用已不在副本中时同样明确失败，不猜测字节数。
 */
export function chatPayloadReader(source: () => PayloadSource | undefined): ToolPayloadReader {
  return async ({ id, signal }) => {
    const channel = source();
    if (!channel) throw new PayloadReadError('unavailable', '会话尚未就绪，无法读取全文。');
    try {
      return await channel.readPayloadById(id, signal);
    } catch (error) {
      if (error instanceof PayloadReadError) throw error;
      throw new PayloadReadError('unavailable', error instanceof Error ? error.message : '全文读取失败。');
    }
  };
}

type PayloadValueResult = { ok: true; value: unknown } | { ok: false; error: string };

/** 读取并校验全文，保留原始 JSON 值供展示层按领域语义格式化。 */
export async function loadPayloadValue(
  ref: ToolPayloadReadRef,
  reader: ToolPayloadReader = unavailablePayloadReader,
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
