import type { HistoryPageOutcome } from "../agent/port";
import type { AcpMcpServer } from "../contracts/acp";
import type {
    ChatBindingTarget,
    ChatDatabase,
    ChatFactObserver,
    ChatSessionDirectoryEntry,
} from "../contracts/chat";
import type {
    ReadPayloadOptions, ToolPayloadReadRef, ToolPayloadResponse,
} from "../payload/reader";
import type { Transport } from "../transport/types";
import type { WasmRuntimeDiagnosticKind } from "../wasm/runtime";
import type { PeriWasmModuleFactory } from "../wasm/loader";
import type {
    AnswerAction, CancelAction, ForceSendAction, HistoryAction, SendAction, SessionAction, TakeBackAction,
} from "./action";

/**
 * Chat 控制面与 View 的冻结契约（Phase 0）。
 *
 * `ChatManager`/`ChatService` 在 Phase 2 由 `src/chat/{manager,service}.ts` 实现；
 * 这里先固定方法、结果与依赖边界，避免 Phase 1-3 并行时各自发明入口。
 */

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

/**
 * 宿主提供的 KV 存储端口。
 *
 * 只要求 3 个方法；unstorage 的 `Storage` 结构化满足该端口。SDK 不 import
 * unstorage，也不做 TTL/续租/CAS。
 */
export interface KvStorage {
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

/** Session 建立输入：新会话 cwd、系统指令与 MCP 声明。 */
export interface ChatSessionInit {
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

/** Chat 运行配置：运行后端、Session 建立输入与生命周期预算。 */
export interface ChatConfig {
    runtime: ChatRuntime;
    /** Manager 级默认 Session 建立输入；`createChat({ sessionInit })` 整体覆盖。 */
    sessionInit?: ChatSessionInit;
    lifecycle?: ChatLifecycleOptions;
}

/** `createChat` 的句柄级配置；`sessionInit` 整体覆盖 Manager 默认值，不做字段合并。 */
export interface ChatCreateOptions {
    path?: string;
    sessionInit?: ChatSessionInit;
}

export interface ChatManagerOptions {
    /** 纯 JSON 配置；构造时序列化快照，拒绝函数/循环/不可序列化值。 */
    agentConfig: unknown;
    database: ChatDatabase;
    /** Session 归属记录存储；Manager 内部构造 ownership 并生成 owner。 */
    kv: KvStorage;
    /** Chat 运行配置；runtime 必填。 */
    chatConfig: ChatConfig;
}

export interface ChatManager {
    /** 同步声明句柄；不启动 ACP、不取得 Session ownership。 */
    createChat(options?: ChatCreateOptions): ChatService;
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
