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
import type { PeriInstanceOptions } from "../agent/peri-instance";
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

/**
 * Manager 部署边界：整个 Manager 共享的 Store 与机器身份。
 *
 * 由 `ChatManagerOptions.store` 提供，构造时快照；SDK 在序列化每个 Chat 的
 * 内部 `agentConfig` JSON 时注入 `storage`/`machineId`，per-chat 不能声明这两个键。
 */
export interface ChatStoreConfig {
    storage: unknown;
    machineId: string;
}

/**
 * 单个 Chat 的运行配置。
 *
 * `cwd` 必填；`settings`/`env` 与其余 JSON 字段可选。`storage`/`machineId`
 * 由 Manager 的 `store` 注入，出现在 per-chat 配置里会抛 `TypeError`。
 */
export interface ChatAgentConfig {
    cwd: string;
    settings?: unknown;
    env?: Readonly<Record<string, string>>;
    [key: string]: unknown;
}

/** Session 建立输入：新会话 cwd、系统指令与 MCP 声明；嵌套容器按 Chat 深拷贝。 */
export interface ChatSessionInit {
    path?: string;
    instructions?: string;
    /** `acp` 型 server 实例由调用方持有；SDK 复制描述对象但不克隆实例。 */
    mcpServers?: Record<string, AcpMcpServer>;
}

/** 宿主 transport 工厂每次激活收到的上下文；旧工厂可忽略新增字段。 */
export interface ChatTransportFactoryOptions {
    signal?: AbortSignal;
    /** 当前激活所属的 Chat 句柄 id。 */
    chatId?: string;
    /** 该 Chat 独立组合、冻结后的内部 agentConfig JSON（含 Manager store）。 */
    agentConfig?: unknown;
}

export type ChatRuntime =
    | {
          kind: "wasm";
          moduleUrl?: string | URL;
          moduleFactory?: PeriWasmModuleFactory;
          /** Manager 级默认 WASM env；`createChat({ runtime })` 按字段合并覆盖。 */
          env?: Readonly<Record<string, string>>;
          /** Manager 级诊断回调；per-chat 未提供时沿用。 */
          onDiagnostic?: (kind: WasmRuntimeDiagnosticKind, error: unknown) => void;
      }
    | {
          kind: "transport";
          factory: (
              path: string,
              options?: ChatTransportFactoryOptions,
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
    /** shutdown 等待单个 chat.close；Manager 聚合预算，per-chat 覆盖不生效。默认 15000ms。 */
    shutdownChatTimeoutMs?: number;
}

/** Chat 运行配置：运行后端、Session 建立输入与生命周期预算。 */
export interface ChatConfig {
    runtime: ChatRuntime;
    /** Manager 级默认 Session 建立输入；`createChat({ session })` 整体覆盖。 */
    session?: ChatSessionInit;
    /** Manager 级默认生命周期预算；`createChat({ lifecycle })` 按字段覆盖。 */
    lifecycle?: ChatLifecycleOptions;
}

/**
 * `createChat` 的句柄级配置。
 *
 * Manager 仍是部署/Store 边界；这里提供单个 Chat 的运行配置与 Session 输入。
 * `agent` 必填，最终配置 = 本对象 + 注入的 Manager store，每个 Chat 独立。
 * `session` 整体覆盖 Manager 默认值，不做字段合并。
 */
export interface ChatCreateOptions {
    /** Per-chat 运行配置；`cwd` 必填，不能声明 `storage`/`machineId`。 */
    agent: ChatAgentConfig;
    path?: string;
    session?: ChatSessionInit;
    /**
     * Partial lifecycle overrides merged over the Manager defaults.
     * `shutdownChatTimeoutMs` stays Manager-scoped.
     */
    lifecycle?: ChatLifecycleOptions;
    /** Per-chat WASM runtime settings; env is merged over chatConfig.runtime.env. */
    runtime?: {
        env?: Readonly<Record<string, string>>;
        /** Overrides the Manager diagnostic handler for this chat's runtime. */
        onDiagnostic?: (kind: WasmRuntimeDiagnosticKind, error: unknown) => void;
    };
    /** Passed to each PeriInstance; no Manager default. */
    payloadBudgets?: PeriInstanceOptions["payloadBudgets"];
}

export interface ChatManagerOptions {
    /**
     * Manager 级 Store 与机器身份；构造时校验并快照。
     * 序列化每个 Chat 的内部 `agentConfig` JSON 时注入 `storage`/`machineId`。
     */
    store: ChatStoreConfig;
    database: ChatDatabase;
    /** Session 归属记录存储；Manager 内部构造 ownership 并生成 owner。 */
    kv: KvStorage;
    /** Chat 运行配置；runtime 必填。 */
    chatConfig: ChatConfig;
}

export interface ChatManager {
    /** 同步声明句柄；不启动 ACP、不取得 Session ownership。 */
    createChat(options: ChatCreateOptions): ChatService;
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
