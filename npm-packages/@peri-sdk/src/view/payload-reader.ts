/** 默认待确认（设计评审起点）：浏览器单次载荷展示的内存上限。 */
export const VIEW_PAYLOAD_MAX_BYTES = 1_048_576;
/** 默认待确认（设计评审起点）：单个浏览器的并发载荷读取上限。 */
export const VIEW_PAYLOAD_MAX_CONCURRENCY = 2;

export type PayloadReadErrorCode =
    | "unauthorized" | "unavailable" | "too_large" | "integrity_error" | "aborted" | "transport_error";

export class PayloadReadError extends Error {
    constructor(readonly code: PayloadReadErrorCode, message: string, options?: { cause?: unknown }) {
        super(message, options);
        this.name = "PayloadReadError";
    }
}

export type ToolPayloadReadRef = { id: string; bytes: number };
export type ToolPayloadResponse = { id: string; bytes: number; json: string };
/** 注入的 transport-neutral reader；服务端 adapter 用 SessionDocs.readPayload，浏览器 adapter 走鉴权 HTTP。 */
export type ToolPayloadReader = (request: { id: string; signal?: AbortSignal }) => Promise<ToolPayloadResponse>;
export type ReadPayloadOptions = { signal?: AbortSignal; maxBytes?: number };

const encoder = new TextEncoder();

/**
 * 校验一次载荷读取：id 与字节数必须和引用一致，正文必须是合法 JSON。
 * reader 抛出的 PayloadReadError 原样透传；其余失败归 transport_error。
 */
export async function readPayload(
    ref: ToolPayloadReadRef,
    reader: ToolPayloadReader,
    options: ReadPayloadOptions = {},
): Promise<ToolPayloadResponse> {
    const maxBytes = options.maxBytes ?? VIEW_PAYLOAD_MAX_BYTES;
    if (options.signal?.aborted) throw new PayloadReadError("aborted", "载荷读取已取消");
    if (ref.bytes > maxBytes) {
        throw new PayloadReadError("too_large", `载荷 ${ref.bytes} 字节超过单次展示上限 ${maxBytes}，请走鉴权下载`);
    }
    let response: ToolPayloadResponse;
    try {
        response = await reader({ id: ref.id, signal: options.signal });
    } catch (error) {
        if (error instanceof PayloadReadError) throw error;
        if (options.signal?.aborted) throw new PayloadReadError("aborted", "载荷读取已取消", { cause: error });
        throw new PayloadReadError("transport_error", "载荷读取失败", { cause: error });
    }
    if (options.signal?.aborted) throw new PayloadReadError("aborted", "载荷读取已取消");
    if (!response || response.id !== ref.id || typeof response.json !== "string") {
        throw new PayloadReadError("integrity_error", "载荷响应与请求引用不一致");
    }
    const bytes = encoder.encode(response.json).byteLength;
    if (bytes > maxBytes) {
        throw new PayloadReadError("too_large", `载荷 ${bytes} 字节超过单次展示上限 ${maxBytes}，请走鉴权下载`);
    }
    if (bytes !== ref.bytes || response.bytes !== ref.bytes) {
        throw new PayloadReadError("integrity_error", `载荷字节数与引用不符 (${response.bytes}/${ref.bytes}/${bytes})`);
    }
    try { JSON.parse(response.json); }
    catch (error) { throw new PayloadReadError("integrity_error", "载荷不是合法 JSON", { cause: error }); }
    return { id: ref.id, bytes, json: response.json };
}

/** 浏览器侧并发闸门：最多 maxConcurrency 个在途读取，其余按 FIFO 排队。 */
export function limitPayloadReads(
    reader: ToolPayloadReader,
    maxConcurrency: number = VIEW_PAYLOAD_MAX_CONCURRENCY,
): ToolPayloadReader {
    const limit = Math.max(1, Math.floor(maxConcurrency));
    let active = 0;
    const waiting: Array<() => void> = [];
    return async (request) => {
        if (active >= limit) await new Promise<void>((resolve) => waiting.push(resolve));
        active++;
        try { return await reader(request); }
        finally {
            active--;
            waiting.shift()?.();
        }
    };
}
