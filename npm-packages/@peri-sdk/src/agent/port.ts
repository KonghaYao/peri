import type { AcpMcpServer } from "../contracts/acp";
import type { ChatFact } from "../contracts/chat";
import type { InteractionAnswer } from "../contracts/interaction";
import type {
    ReadPayloadOptions, ToolPayloadReadRef, ToolPayloadResponse,
} from "../payload/reader";

export interface HistoryPageOutcome {
    applied: number;
    nextCursor: string | null;
}

/** ChatService 与 PeriInstance 之间的内部端口；Phase 1/2 平行开发时以此为准。 */
export interface PeriInstancePort {
    /** 先同步挂 sink，再 bind；bind 期间的初始化事实也不能丢。 */
    attach(sink: (fact: ChatFact) => void): () => void;
    bindNew(options: {
        path: string;
        title?: string;
        instructions?: string;
        mcpServers?: Record<string, AcpMcpServer>;
        /** `session/new` 返回后、后续阶段前回调；用于先登记索引与 ownership。 */
        onSessionCreated?: (sessionId: string) => void | Promise<void>;
    }): Promise<{ sessionId: string }>;
    bindExisting(sessionId: string): Promise<{ sessionId: string }>;
    send(inputId: string, text: string): Promise<void>;
    waitForDelivery(inputId: string): Promise<void>;
    dispatch(inputId: string): Promise<void>;
    takeBack(inputId: string): Promise<void>;
    cancel(): Promise<"requested" | "idle">;
    answer(interactionId: string, answer: InteractionAnswer): Promise<void>;
    loadEarlierHistory(limit?: number): Promise<HistoryPageOutcome>;
    /**
     * 只读运行侧载荷仓库。
     *
     * 契约语义：
     * - 未绑定、已关闭或无可读载荷：`PayloadReadError("unavailable")`；
     * - 引用字节数与存储不一致或非法 JSON：`integrity_error`；
     * - 超过 `maxBytes`：`too_large`；`signal.aborted`：`aborted`。
     */
    readPayload(
        ref: ToolPayloadReadRef,
        options?: ReadPayloadOptions,
    ): Promise<ToolPayloadResponse>;
    close(): Promise<void>;
    readonly running: boolean;
    readonly pendingInputCount: number;
    readonly pendingInteractionCount: number;
    /** 资源已不可继续使用（关闭未确认/旧运行时已释放）；ChatService 据此拒绝新执行。 */
    readonly needsReload: boolean;
}
