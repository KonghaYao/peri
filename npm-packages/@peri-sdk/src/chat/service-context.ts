import type {
    WasmRuntimeAcquireOptions,
    WasmRuntimeLease,
} from "../wasm/runtime-pool";
import type { PeriInstancePort } from "../agent/port";
import type { ChatDatabase, ChatFact } from "../contracts/chat";
import type { WasmRuntimeDiagnosticKind } from "../wasm/runtime";
import type { PeriInstanceOptions } from "../agent/peri-instance";
import type { WorkspaceRef } from "../workspace/types";
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
    /** 该 Chat 独立组合、冻结后的 agentConfig（含 Manager store）。 */
    readonly agentConfig: unknown;
    /** 该 Chat 独立的序列化快照；WASM 启动配置按 Chat 独立。 */
    readonly agentConfigJson: string;
    readonly sessionInit: ChatSessionInit;
    readonly runtime: ChatRuntime;
    /** 该 Chat 的 WASM env（Manager 默认 + per-chat 合并）。 */
    readonly runtimeEnv?: Readonly<Record<string, string>>;
    /** 该 Chat 的 WASM 诊断回调。 */
    readonly onRuntimeDiagnostic?: (
        kind: WasmRuntimeDiagnosticKind,
        error: unknown,
    ) => void;
    /** 该 Chat 的载荷预算；未配置时 PeriInstance 使用自身默认值。 */
    readonly payloadBudgets?: PeriInstanceOptions["payloadBudgets"];
    readonly signal: AbortSignal;
    readonly database: ChatDatabase;
    readonly lifecycle: Required<ChatLifecycleOptions>;
    /** 仅 runtime.kind === "wasm" 可用；租约由 PeriInstance 关闭并检查 confirmed。 */
    acquireWasmRuntime(
        options?: WasmRuntimeAcquireOptions,
    ): Promise<WasmRuntimeLease>;
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
    /** 该 Manager 内已知的 Session workspace；持久 Session 可能没有记录。 */
    workspaceOf(sessionId: string): string | undefined;
    /** 记录 Session 所属 workspace；同一 Session 不允许改写。 */
    claimWorkspace(sessionId: string, workspaceId: string): void;
}

export interface ChatServiceOptions {
    readonly id: string;
    /** 该 Chat 的 Workspace；`workspace.id` 用于跨 workspace 判定，`runtimePath` 是根。 */
    readonly workspace: WorkspaceRef;
    /** 由 workspace 与 sessionPath 派生的新会话 cwd。 */
    readonly sessionCwd: string;
    readonly sessionInit: ChatSessionInit;
    /** 该 Chat 独立组合、冻结后的 agentConfig（含 Manager store）。 */
    readonly agentConfig: unknown;
    readonly agentConfigJson: string;
    readonly runtime: ChatRuntime;
    readonly runtimeEnv?: Readonly<Record<string, string>>;
    readonly onRuntimeDiagnostic?: (
        kind: WasmRuntimeDiagnosticKind,
        error: unknown,
    ) => void;
    readonly payloadBudgets?: PeriInstanceOptions["payloadBudgets"];
    readonly database: ChatDatabase;
    readonly ownership: SessionOwnership;
    readonly index: ChatSessionIndex;
    readonly lifecycle?: ChatLifecycleOptions;
    /** Manager 默认装配真实 PeriInstance；测试注入 fake port。 */
    readonly instanceFactory: ChatInstanceFactory;
    acquireWasmRuntime(
        options?: WasmRuntimeAcquireOptions,
    ): Promise<WasmRuntimeLease>;
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
