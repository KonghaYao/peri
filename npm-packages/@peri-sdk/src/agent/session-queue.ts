import { RpcError } from "../transport/rpc-error";

/** Peri 把所有 mailbox 错误统一映射为该 JSON-RPC 码；只能靠 `data` 区分。 */
const QUEUE_REJECTED_CODE = -32602;

/**
 * 输入队列已换代：Peri 在持久化不一致后丢弃 mailbox，下一次请求新建换代队列。
 *
 * 旧代次的写入不可再信；宿主必须重建 runtime（新进程 / WASM 实例）后重新
 * `session/load`，不能只重试输入。SDK 也据此不自动重放用户内容。
 */
export class SessionQueueStaleError extends Error {
    /** 宿主必须重建 runtime 后重新加载会话。 */
    readonly requiresRuntimeRestart = true;

    constructor(
        /** 服务端最新 generation。 */
        readonly generation: string,
        /** 触发换代的队列快照；可直接用于更新投影。 */
        readonly snapshot: unknown,
        message: string,
        options?: { cause?: unknown },
    ) {
        super(message, options);
        this.name = "SessionQueueStaleError";
    }
}

/** 从队列快照读取非空 generation；非法快照返回 null。 */
export function queueGenerationOf(snapshot: unknown): string | null {
    if (snapshot === null || typeof snapshot !== "object" || Array.isArray(snapshot)) return null;
    const generation = (snapshot as { generation?: unknown }).generation;
    return typeof generation === "string" && generation.length > 0 ? generation : null;
}

/**
 * 队列拒绝回执携带的快照。
 *
 * `rejected === true` 与 `snapshot` 缺一即不是可判定的队列事实，保持原错误。
 */
export function rejectedQueueSnapshot(error: unknown): unknown {
    if (!(error instanceof RpcError) || error.code !== QUEUE_REJECTED_CODE) return null;
    const data = error.data;
    if (data === null || typeof data !== "object" || Array.isArray(data)) return null;
    const value = data as Record<string, unknown>;
    if (value.rejected !== true) return null;
    return value.snapshot ?? null;
}
