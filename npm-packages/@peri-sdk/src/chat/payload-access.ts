import {
    PayloadReadError,
    readPayload as readAndValidatePayload,
    type PayloadReadErrorCode,
    type ReadPayloadOptions,
    type ToolPayloadReadRef,
    type ToolPayloadResponse,
} from "../payload/reader";
import type { PeriInstancePort } from "../agent/port";

const PAYLOAD_ERROR_CODES: ReadonlySet<string> = new Set<PayloadReadErrorCode>([
    "unauthorized",
    "unavailable",
    "too_large",
    "integrity_error",
    "aborted",
    "transport_error",
]);

/** 保留运行侧已有的载荷错误码；未知失败交给通用读取器归类。 */
function normalizePayloadError(error: unknown): unknown {
    if (error instanceof PayloadReadError) return error;
    const code = (error as { code?: unknown })?.code;
    if (typeof code === "string" && PAYLOAD_ERROR_CODES.has(code)) {
        return new PayloadReadError(
            code as PayloadReadErrorCode,
            error instanceof Error ? error.message : String(error),
            { cause: error },
        );
    }
    return error;
}

/**
 * 读取运行侧载荷。
 *
 * `readPayload` 是 `PeriInstancePort` 的必需能力：缺失是类型错误，不再做运行时
 * 能力探测。返回值继续经过 `readPayload` 的 id/字节数/合法 JSON/maxBytes 校验。
 */
export async function readChatPayload(
    port: PeriInstancePort | undefined,
    ref: ToolPayloadReadRef,
    options?: ReadPayloadOptions,
): Promise<ToolPayloadResponse> {
    if (!port) {
        throw new PayloadReadError(
            "unavailable",
            "Chat has no active binding for payload reads",
        );
    }
    return readAndValidatePayload(
        ref,
        async (request) => {
            try {
                return await port.readPayload(
                    { id: request.id, bytes: ref.bytes },
                    options,
                );
            } catch (error) {
                throw normalizePayloadError(error);
            }
        },
        options,
    );
}
