import type { InteractionAnswer } from "../agent/interaction-responder";
import type { AcpMcpServer } from "../agent/types";
import type { Transport } from "../transport/types";
import type {
    ReadPayloadOptions, ToolPayloadReadRef, ToolPayloadResponse,
} from "../view/payload-reader";
import type {
    WasmRuntimeDiagnosticKind, WasmRuntimeLifecycleEvent, WasmRuntimePorts,
} from "../wasm/runtime";
import type { PeriWasmModuleFactory } from "../wasm/loader";
import type {
    AnswerAction, CancelAction, ForceSendAction, HistoryAction, SendAction, SessionAction, TakeBackAction,
} from "./action";
import type { ChatFact, ChatFactObserver } from "./facts";

/**
 * Chat 控制面与 View 的冻结契约（Phase 0）。
 *
 * `ChatManager`/`ChatService` 在 Phase 2 由 `src/chat/{manager,service}.ts` 实现；
 * 这里先固定方法、结果与依赖边界，避免 Phase 1-3 并行时各自发明入口。
 */

export interface ChatBindingTarget {
    chatId: string;
    sessionId: string;
    bindingRevision: number;
}

export interface SessionReadyResult extends ChatBindingTarget {
    requestId: string;
}

/** 同步取得 inputId，`then` 等待投递；`accepted` 只表示入队被接受。 */
export interface SendReceipt extends PromiseLike<void> {
    readonly requestId: string;
    readonly inputId: string;
    readonly target: ChatBindingTarget;
    readonly accepted: Promise<void>;
    readonly isSent: boolean;
}

export interface CancelRequestResult {
    requestId: string;
    status: "requested" | "idle";
}

export interface ControlResult {
    requestId: string;
    status: "accepted";
}

export interface HistoryPageOutcome {
    applied: number;
    nextCursor: string | null;
}

export interface ChatSessionDirectoryEntry {
    sessionId: string;
    title: string | null;
    cwd: string;
    messageCount: number;
    createdAt: string;
    updatedAt: string;
}

/** 只读会话目录；`path` 省略时交给适配器决定（SDK 默认实现不猜 cwd）。 */
export interface ChatDatabase {
    listSessions(path?: string): Promise<ChatSessionDirectoryEntry[]>;
    getSession(id: string): Promise<ChatSessionDirectoryEntry | null>;
}

/**
 * 宿主提供的 Session ownership 存储端口。
 *
 * 只要求 3 个方法；unstorage 的 `Storage` 结构化满足该端口。SDK 不 import
 * unstorage，也不做 TTL、续租或 CAS。读后写非原子：同一 Session 的单实例
 * 绑定由路由层串行化保证。
 */
export interface SessionOwnershipStorage {
    getItem(key: string): Promise<unknown>;
    setItem(key: string, value: unknown): Promise<void>;
    removeItem(key: string): Promise<void>;
}

/**
 * Manager 内部的 Session 执行权协调器。
 *
 * 由 `ChatManager` 为每个实例构造独立的 ownership（owner 为内部随机
 * UUID），避免多个 Manager 共享同一个 owner 身份。此接口不对外导出。
 */
export interface SessionOwnership {
    acquire(sessionId: string): Promise<void>;
    release(sessionId: string): Promise<void>;
}

export interface ChatDefaults {
    path?: string;
    instructions?: string;
    /** `acp` 型 server 实例由调用方持有；Manager 不深拷贝/冻结实例。 */
    mcpServers?: Record<string, AcpMcpServer>;
}

export type ChatRuntime =
    | {
          kind: "wasm";
          moduleUrl?: string | URL;
          moduleFactory?: PeriWasmModuleFactory;
          env?: Readonly<Record<string, string>>;
          moduleOptions?: Readonly<Record<string, unknown>>;
          ports?: WasmRuntimePorts;
          cleanupTimeoutMs?: number;
          startupTimeoutMs?: number;
          onLifecycle?: (event: WasmRuntimeLifecycleEvent) => void;
          onDiagnostic?: (kind: WasmRuntimeDiagnosticKind, error: unknown) => void;
      }
    | {
          kind: "transport";
          factory: (
              path: string,
              options?: { signal?: AbortSignal },
          ) => Transport | Promise<Transport>;
      };

/**
 * 关闭/启动的有界等待预算；单一权威，调用方不再各自发明超时。
 *
 * 超时一律表达为 Incomplete：保留 ownership、索引与资源供重试，不产生 `closed` 事实。
 */
export interface ChatLifecycleOptions {
    /** session/close 有界重放预算；默认 3000ms。 */
    closeDrainTimeoutMs?: number;
    /** 中止启动后等待绑定调用结算；默认 5000ms。 */
    startupSettleTimeoutMs?: number;
    /** 单个激活（宿主/transport）关闭等待；默认 5000ms。 */
    activationCloseTimeoutMs?: number;
    /** shutdown 等待单个 chat.close；默认 15000ms。 */
    shutdownChatTimeoutMs?: number;
}

/** Chat 运行配置：运行后端、默认值与生命周期预算。 */
export interface ChatConfig {
    runtime: ChatRuntime;
    defaults?: ChatDefaults;
    lifecycle?: ChatLifecycleOptions;
}

export interface ChatManagerOptions {
    /** 纯 JSON 配置；构造时序列化快照，拒绝函数/循环/不可序列化值。 */
    agentConfig: unknown;
    database: ChatDatabase;
    /** Session 归属记录存储；Manager 内部构造 ownership 并生成 owner。 */
    sessionOwnershipStorage: SessionOwnershipStorage;
    /** Chat 运行配置；runtime 必填。 */
    chatConfig: ChatConfig;
}

export interface ChatManager {
    /** 同步声明句柄；不启动 ACP、不取得 Session ownership。 */
    createChat(options?: { path?: string }): ChatService;
    getChat(id: string): ChatService | undefined;
    /** 包含 binding/cleanup 中的候选句柄。 */
    findChatBySession(sessionId: string): ChatService | undefined;
    listSessions(options?: { path?: string }): Promise<ChatSessionDirectoryEntry[]>;
    /** 成功清理后才移除；未知 id 为 no-op。 */
    closeChat(id: string): Promise<void>;
    /** 先封新准入，再聚合清理；可重试。 */
    shutdown(): Promise<void>;
}

export interface ChatService {
    readonly id: string;
    readonly sessionId: string | null;
    readonly bindingRevision: number;
    readonly binding: ChatBindingTarget | null;

    execute(action: SendAction): SendReceipt;
    execute(action: SessionAction): Promise<SessionReadyResult>;
    execute(action: CancelAction): Promise<CancelRequestResult>;
    execute(action: ForceSendAction | TakeBackAction | AnswerAction): Promise<ControlResult>;
    execute(action: HistoryAction): Promise<HistoryPageOutcome>;

    readPayload(
        ref: ToolPayloadReadRef,
        options?: ReadPayloadOptions,
    ): Promise<ToolPayloadResponse>;

    close(): Promise<void>;
    /** 包内事实接入；公开文档标记 internal。 */
    subscribeFacts(observer: ChatFactObserver): () => void;
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
