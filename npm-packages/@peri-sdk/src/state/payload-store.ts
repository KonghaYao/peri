import {
    SESSION_EXTERNAL_PAYLOAD_BUDGET_BYTES,
    SESSION_INLINE_PAYLOAD_BUDGET_BYTES,
    TOOL_PAYLOAD_INLINE_BYTES,
    TOOL_PAYLOAD_MAX_BYTES,
    TOOL_PAYLOAD_PREVIEW_CHARS,
    ToolPayloadCapacityError,
    type ToolPayloadBudgets,
    type ToolPayloadRef,
} from "./tool-payloads";

/**
 * 运行侧工具载荷仓库（无 Yjs）。
 *
 * 大值只在本实例内保存，事实里写 `{id, bytes, preview}`；ref id 由
 * `binding nonce + field + JSON hash` 内容寻址，所以同一内容在实时通知、
 * 历史回放和多个 View 中得到同一个 id。超出单次上限或外置预算时显式抛
 * `ToolPayloadCapacityError`，调用方不写该字段、保留上一个可读版本。
 */

const encoder = new TextEncoder();
const bytesOf = (json: string): number => encoder.encode(json).byteLength;

export type PayloadField = "arguments" | "result";

type NormalizedPayload =
    | { kind: "inline"; value: unknown }
    | { kind: "external"; ref: ToolPayloadRef };

type PayloadStoreErrorCode =
    | "unavailable"
    | "too_large"
    | "integrity_error"
    | "invalid_json";

export class PayloadStoreError extends Error {
    constructor(readonly code: PayloadStoreErrorCode, message: string) {
        super(message);
        this.name = "PayloadStoreError";
    }
}

/** 64 位 FNV-1a 风格内容摘要（两个 32 位种子合并为 16 位十六进制）。 */
function contentHash(json: string): string {
    let first = 0x811c9dc5;
    let second = 0x9e3779b9;
    for (let index = 0; index < json.length; index++) {
        const code = json.charCodeAt(index);
        first ^= code;
        first = Math.imul(first, 0x01000193) >>> 0;
        second = Math.imul(second ^ code, 0x85ebca6b) >>> 0;
    }
    return first.toString(16).padStart(8, "0") + second.toString(16).padStart(8, "0");
}

export class PayloadStore {
    private readonly nonce: string;
    private readonly inlineBudgetBytes: number;
    private readonly externalBudgetBytes: number;
    private readonly maxPayloadBytes: number;
    private readonly values = new Map<string, string>();
    /** 已计入 inline 预算的内容，避免同一内容重复计费。 */
    private readonly inlineCharged = new Set<string>();
    private inlineBytes = 0;
    private externalBytes = 0;

    constructor(budgets: ToolPayloadBudgets = {}, nonce: string = crypto.randomUUID()) {
        this.nonce = nonce;
        this.inlineBudgetBytes = budgets.inlineBytes ?? SESSION_INLINE_PAYLOAD_BUDGET_BYTES;
        this.externalBudgetBytes = budgets.externalBytes ?? SESSION_EXTERNAL_PAYLOAD_BUDGET_BYTES;
        this.maxPayloadBytes = budgets.maxPayloadBytes ?? TOOL_PAYLOAD_MAX_BYTES;
    }

    /** 保存一份 JSON 值：小且预算内留在事实里，否则返回外部引用。 */
    normalize(field: PayloadField, value: unknown): NormalizedPayload {
        const json = JSON.stringify(value);
        if (json === undefined) throw new TypeError("Tool payload must be JSON serializable");
        const bytes = bytesOf(json);
        if (bytes <= TOOL_PAYLOAD_INLINE_BYTES
            && this.inlineBytes + (this.inlineCharged.has(json) ? 0 : bytes) <= this.inlineBudgetBytes) {
            if (!this.inlineCharged.has(json)) {
                this.inlineCharged.add(json);
                this.inlineBytes += bytes;
            }
            return { kind: "inline", value: JSON.parse(json) as unknown };
        }
        if (bytes > this.maxPayloadBytes)
            throw new ToolPayloadCapacityError(`工具载荷 ${bytes} 字节超过单次接纳上限 ${this.maxPayloadBytes}`);
        if (this.externalBytes + (this.values.has(this.idFor(field, json)) ? 0 : bytes) > this.externalBudgetBytes)
            throw new ToolPayloadCapacityError(`工具载荷将超出会话外置预算 ${this.externalBudgetBytes}`);
        const id = this.idFor(field, json);
        const existing = this.values.get(id);
        if (existing !== undefined && existing !== json)
            throw new PayloadStoreError("unavailable", `Payload content hash collision for ${id}`);
        if (existing === undefined) {
            this.values.set(id, json);
            this.externalBytes += bytes;
        }
        return { kind: "external", ref: { id, bytes, preview: this.preview(field, value, json) } };
    }

    /** 读取精确 JSON；id、字节数、合法 JSON 与 `maxBytes` 都要校验。 */
    read(ref: { id: string; bytes: number }, options: { maxBytes?: number } = {}): string {
        const json = this.values.get(ref.id);
        if (json === undefined) throw new PayloadStoreError("unavailable", `Unknown payload: ${ref.id}`);
        const bytes = bytesOf(json);
        if (bytes !== ref.bytes)
            throw new PayloadStoreError("integrity_error", `Payload bytes changed for ${ref.id}: stored ${bytes}, ref ${ref.bytes}`);
        if (options.maxBytes !== undefined && bytes > options.maxBytes)
            throw new PayloadStoreError("too_large", `Payload ${ref.id} exceeds ${options.maxBytes} bytes`);
        try {
            JSON.parse(json);
        } catch {
            throw new PayloadStoreError("invalid_json", `Payload ${ref.id} is not valid JSON`);
        }
        return json;
    }

    private idFor(field: PayloadField, json: string): string {
        return `payload:${this.nonce}:${field}:${contentHash(json)}`;
    }

    private preview(field: PayloadField, value: unknown, json: string): string {
        if (field === "result" && value && typeof value === "object") {
            const text = (value as Record<string, unknown>).contentText;
            if (typeof text === "string") return text.slice(0, TOOL_PAYLOAD_PREVIEW_CHARS);
        }
        return json.slice(0, TOOL_PAYLOAD_PREVIEW_CHARS);
    }
}
