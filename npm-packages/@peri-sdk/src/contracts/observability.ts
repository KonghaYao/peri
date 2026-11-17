import type { WorkspaceRef } from "../workspace/types";

/**
 * 只读观测快照的公开类型。
 *
 * 快照是对 `ChatManager` + 可选 `SandboxManager` 的一次同步投影，不启动
 * WASM/ACP/sandbox，也不改变任何状态。字段只包含标识、状态与数值；token、
 * header 值、stdio command/args/env 与 URL 凭据不得进入本契约。
 * `lastError.message` 在 handle 与快照边界各经一次共享脱敏，文本中的 URL
 * 收敛为 origin + pathname，但仍不保证任意自由文本绝对无敏感信息。
 */

/** 公开相位；由 ChatService 内部绑定阶段派生，粒度有限。 */
export type ChatObservationPhase = "empty" | "bound" | "closing" | "blocked";

/** 实例生命周期状态；`unknown` 表示 SDK 没有该实例的权威状态。 */
export type ChatInstanceState =
    | "live"
    | "closing"
    | "closed"
    | "close-unconfirmed"
    | "unknown";

export interface ChatInstanceObservation {
    kind: "wasm" | "transport";
    state: ChatInstanceState;
    /** 运行后端就绪的本地时间；不可得时省略。 */
    startedAtMs?: number;
    /** WASM 线性内存当前容量；不可得时省略。 */
    linearMemoryBytes?: number;
    running: boolean;
    pendingInputCount: number;
    pendingInteractionCount: number;
}

/** Sandbox handle 状态；与 `SandboxHandleState` 同源。 */
export type SandboxObservationState =
    | "idle"
    | "starting"
    | "ready"
    | "stopping"
    | "stopped"
    | "failed"
    | "closed";

/** Chat 观测只需要 workspace 的身份与路径边界。 */
export interface ObservedWorkspaceRef {
    id: string;
    runtimePath: string;
}

export interface McpServerObservation {
    name: string;
    type: "http" | "stdio" | "acp";
    /** `sandbox` 仅表示 endpoint 与已注入的 sandbox 匹配，不代表健康检查通过。 */
    source: "sandbox" | "config";
    /** 安全 endpoint：origin + pathname，不含 userinfo/query/hash。 */
    endpoint?: string;
    /** 只含 header 名称；值不进入快照。 */
    headerNames?: string[];
    health: "unknown";
}

export interface ChatMcpObservation {
    /**
     * `unknown` 表示 SDK 不持有权威 MCP 配置：Chat 尚未创建/绑定会话，或
     * `bindExisting` 之后；此时 `servers` 为空，不伪造服务列表。
     */
    source: "session-init" | "unknown";
    servers: McpServerObservation[];
}

export interface ChatObservation {
    chatId: string;
    sessionId: string | null;
    bindingRevision: number;
    phase: ChatObservationPhase;
    /** 固定为 true：公开相位不是内部状态机的完整投影。 */
    phaseLimited: true;
    workspace: ObservedWorkspaceRef;
    instance: ChatInstanceObservation | null;
    mcp: ChatMcpObservation;
}

export interface SandboxObservation {
    sandboxId: string;
    state: SandboxObservationState;
    serverName: string;
    /** 安全 endpoint；未 ready 时为 null。 */
    mcpEndpoint: string | null;
    workspace: WorkspaceRef | null;
    /** `message` 经共享脱敏；文本中的 URL 收敛为 origin + pathname。 */
    lastError: { code: string; message: string } | null;
}

export interface AgentObservation {
    /** 当前以 chatId 作为 Agent 标识：一个 Chat 对应一个 Agent 节点。 */
    agentId: string;
    chat: ChatObservation;
    /** 与 Chat 的 MCP endpoint 匹配的 sandbox；同一 sandbox 可被多个 Agent 共享。 */
    sandbox: SandboxObservation | null;
}

export interface BackendSnapshotTotals {
    agents: number;
    chats: number;
    /** instance 非空且 state 为 `live` 的 Chat 数。 */
    activeInstances: number;
    mcpServers: number;
    /** 其中 endpoint 与已注入 sandbox 匹配的 MCP server 数。 */
    sandboxMcpServers: number;
    sandboxes: number;
}

export interface BackendSnapshot {
    /** 采样时间（ISO 8601）。 */
    observedAt: string;
    /** 按 agentId 稳定排序。 */
    agents: AgentObservation[];
    /**
     * 未被任何 Chat 的 MCP endpoint 引用的 sandbox，按 sandboxId 稳定排序。
     *
     * 未 ready（没有 endpoint）的 sandbox 也会落在这里；`bindExisting` 的
     * Chat 不暴露 MCP 配置，其实际使用的 sandbox 也可能落在这里。孤儿只是
     * 观测盲区，不是“确定闲置”的证明。
     */
    orphanSandboxes: SandboxObservation[];
    totals: BackendSnapshotTotals;
}

/** Manager 能提供 Chat 观测的最小结构；`ChatManager.observe()` 满足。 */
export interface ChatObservationSource {
    observe(): ChatObservation[];
}

/** Manager 能提供 Sandbox 观测的最小结构；`SandboxManager.snapshot()` 满足。 */
export interface SandboxObservationSource {
    snapshot(): SandboxObservation[];
}

/**
 * Manager 从 WASM 运行池取到的实例补充信息。
 *
 * `ChatService.observe()` 读不到 per-chat 运行池条目；由 Manager 按 chatId
 * 合并，避免 ChatService 依赖 Manager。
 */
export interface ChatInstanceObservationSeed {
    state: ChatInstanceState;
    startedAtMs?: number;
    linearMemoryBytes?: number;
}
