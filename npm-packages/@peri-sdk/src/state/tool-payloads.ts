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
