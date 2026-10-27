import type { WasmRuntimeLease } from "../wasm/runtime-pool";
import type { PeriInstancePort } from "../agent/port";
import type { ChatDatabase, ChatFact } from "../contracts/chat";
import type {
    ChatSessionInit,
    ChatRuntime,
    ChatLifecycleOptions,
    SessionOwnership,
} from "./types";

/** ChatService 构造 PeriInstance 时提供的装配上下文（Phase 4 接入真实实现）。 */
export interface ChatInstanceContext {
    readonly chatId: string;
    readonly path: string;
    /** Manager 构造时的纯 JSON 快照。 */
    readonly agentConfig: unknown;
    readonly sessionInit: ChatSessionInit;
    readonly runtime: ChatRuntime;
    readonly signal: AbortSignal;
    readonly database: ChatDatabase;
    readonly lifecycle: Required<ChatLifecycleOptions>;
    /** 仅 runtime.kind === "wasm" 可用；租约由 PeriInstance 关闭并检查 confirmed。 */
    acquireWasmRuntime(signal?: AbortSignal): Promise<WasmRuntimeLease>;
}

export type ChatInstanceFactory = (
    context: ChatInstanceContext,
) => PeriInstancePort;

/** Manager 提供的本地 Session → 句柄索引；不替代共享 ownership。 */
export interface ChatSessionIndex {
    /** 返回冲突句柄 id；`null` 表示登记成功。 */
    reserve(chatId: string, sessionId: string): string | null;
    commit(
        chatId: string,
        previousSessionId: string | null,
        sessionId: string,
    ): void;
    release(chatId: string, sessionId: string): void;
}

export interface ChatServiceOptions {
    readonly id: string;
    /** createChat({path}) 的句柄级 path；新会话优先使用，加载历史仅兜底。 */
    readonly path?: string;
    readonly sessionInit: ChatSessionInit;
    readonly agentConfig: unknown;
    readonly runtime: ChatRuntime;
    readonly database: ChatDatabase;
    readonly ownership: SessionOwnership;
    readonly index: ChatSessionIndex;
    readonly lifecycle?: ChatLifecycleOptions;
    /** Manager 默认装配真实 PeriInstance；测试注入 fake port。 */
    readonly instanceFactory: ChatInstanceFactory;
    acquireWasmRuntime(signal?: AbortSignal): Promise<WasmRuntimeLease>;
    onClosed(chatId: string): void;
}

export interface Activation {
    readonly port: PeriInstancePort;
    readonly controller: AbortController;
    readonly pending: ChatFact[];
    detach: () => void;
    live: boolean;
    abandoned: boolean;
}

export interface CleanupOutcome {
    readonly confirmed: boolean;
    readonly error?: unknown;
}
