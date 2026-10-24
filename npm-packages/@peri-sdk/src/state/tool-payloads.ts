import type * as Y from "yjs";

/** An immutable JSON version, scoped to one runtime payload store lifetime. */
export type ToolPayloadRef = { id: string; bytes: number; preview: string };
/** 设计固定的 inline 分界；更小的值留在 Yjs，不产生引用。 */
export const TOOL_PAYLOAD_INLINE_BYTES = 4096;
/** 外置引用携带的 preview 字符数。 */
export const TOOL_PAYLOAD_PREVIEW_CHARS = 512;
/** 默认待确认（设计评审起点）：单会话 inline 载荷累计预算，超出后新值降级为外置引用。 */
export const SESSION_INLINE_PAYLOAD_BUDGET_BYTES = 1 * 1024 * 1024;
/** 默认待确认（设计评审起点）：单个外置载荷接纳上限。 */
export const TOOL_PAYLOAD_MAX_BYTES = 8 * 1024 * 1024;
/** 默认待确认（设计评审起点）：单会话外置载荷累计预算，超出时显式拒绝并报告容量错误。 */
export const SESSION_EXTERNAL_PAYLOAD_BUDGET_BYTES = 32 * 1024 * 1024;

export type ToolPayloadBudgets = { inlineBytes?: number; externalBytes?: number; maxPayloadBytes?: number };

/** Projection acceptance refused a payload; the projection reports it as a public capacity error. */
export class ToolPayloadCapacityError extends Error {
    readonly code = "payload_too_large" as const;
    constructor(message: string) { super(message); this.name = "ToolPayloadCapacityError"; }
}

const encoder = new TextEncoder();
const bytesOf = (json: string): number => encoder.encode(json).byteLength;
type Field = "arguments" | "result";

/** Large values live outside the replicated hot document. Rust owns durable history. */
export class ToolPayloads {
    private readonly generation = crypto.randomUUID();
    private nextId = 0;
    private readonly values = new Map<string, string>();
    private inlineBytes = 0;
    private externalBytes = 0;

    constructor(
        private readonly inlineBudgetBytes = SESSION_INLINE_PAYLOAD_BUDGET_BYTES,
        private readonly externalBudgetBytes = SESSION_EXTERNAL_PAYLOAD_BUDGET_BYTES,
        private readonly maxPayloadBytes = TOOL_PAYLOAD_MAX_BYTES,
    ) {}

    read(id: string): string | undefined { return this.values.get(id); }
    clear(): void {
        this.values.clear();
        this.inlineBytes = 0;
        this.externalBytes = 0;
    }

    /** 当前外置版本，按 id 升序；checkpoint 导出用。 */
    snapshot(): Array<{ id: string; bytes: number; json: string }> {
        return [...this.values].map(([id, json]) => ({ id, bytes: bytesOf(json), json }));
    }

    /** checkpoint 导入：只接受已校验引用的版本；重复 id 不重复计数。 */
    restoreExternal(id: string, json: string, bytes: number): void {
        if (!this.values.has(id)) this.externalBytes += bytes;
        this.values.set(id, json);
    }

    /** checkpoint 导入：inline 值同样计入累计预算。 */
    restoreInline(bytes: number): void { this.inlineBytes += bytes; }

    set(tool: Y.Map<unknown>, field: Field, value: unknown): void {
        const json = JSON.stringify(value);
        if (json === undefined) throw new TypeError("Tool payload must be JSON serializable");
        const refKey = `${field}Ref`;
        const previous = tool.get(refKey) as ToolPayloadRef | undefined;
        if (previous && this.values.get(previous.id) === json) return;
        const current = tool.get(field);
        if (!previous && current !== undefined && JSON.stringify(current) === json) return;
        const bytes = bytesOf(json);
        const previousExternal = previous?.bytes ?? 0;
        const previousInline = current === undefined ? 0 : bytesOf(JSON.stringify(current));
        const keepInline = bytes <= TOOL_PAYLOAD_INLINE_BYTES
            && this.inlineBytes - previousInline + bytes <= this.inlineBudgetBytes;
        if (keepInline) {
            // Take ownership: ACP callers must not mutate a value already inserted into Yjs.
            tool.set(field, JSON.parse(json));
            if (previous) {
                this.externalBytes -= previousExternal;
                this.values.delete(previous.id);
                tool.delete(refKey);
            }
            this.inlineBytes = this.inlineBytes - previousInline + bytes;
            return;
        }
        if (bytes > this.maxPayloadBytes) {
            throw new ToolPayloadCapacityError(`工具载荷 ${bytes} 字节超过单次接纳上限 ${this.maxPayloadBytes}`);
        }
        if (this.externalBytes - previousExternal + bytes > this.externalBudgetBytes) {
            throw new ToolPayloadCapacityError(`工具载荷将超出会话外置预算 ${this.externalBudgetBytes}`);
        }
        const id = `${this.generation}:${++this.nextId}`;
        const content = field === "result" && value && typeof value === "object"
            ? (value as Record<string, unknown>).contentText : undefined;
        const preview = (typeof content === "string" ? content : json).slice(0, TOOL_PAYLOAD_PREVIEW_CHARS);
        this.values.set(id, json);
        this.externalBytes = this.externalBytes - previousExternal + bytes;
        tool.set(refKey, { id, bytes, preview } satisfies ToolPayloadRef);
        if (current !== undefined) {
            tool.delete(field);
            this.inlineBytes -= previousInline;
        }
        // Stale references return unavailable, never the content of a different version.
        if (previous) this.values.delete(previous.id);
    }
}
