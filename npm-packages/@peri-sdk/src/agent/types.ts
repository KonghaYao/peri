import type { Sandbox } from "../sandbox/sandbox";

/**
 * 官方 MCP transport 的最小结构契约（对应 `@modelcontextprotocol/sdk` 的 `Transport`）。
 *
 * 只声明桥接真正调用的成员，避免把官方 SDK 变成运行时依赖；官方实例可直接传入。
 */
export interface AcpMcpTransport {
    start(): Promise<void>;
    send(message: unknown, options?: unknown): Promise<void>;
    close(): Promise<void>;
    onclose?: () => void;
    onerror?: (error: Error) => void;
    onmessage?: (message: unknown, extra?: unknown) => void;
    sessionId?: string;
    setProtocolVersion?: (version: string) => void;
}

/** 官方 `Server` / `McpServer` 的最小结构契约：桥接只依赖 `connect(transport)`。 */
export interface AcpMcpServerInstance {
    connect(transport: AcpMcpTransport): Promise<void>;
}

export type AcpMcpServer =
    | { type: "http"; url: string; headers?: Record<string, string> }
    | {
          type: "stdio";
          command: string;
          args?: string[];
          env?: Record<string, string>;
      }
    | {
          /** 由调用方持有生命周期的官方 MCP server 实例，经反向 ACP `mcp/message` 桥接。 */
          type: "acp";
          server: AcpMcpServerInstance;
          /** 缺省使用 `mcpServers` 的 Record key。 */
          serverId?: string;
      };

/** Peri's reverse ACP permission request, with the tool input available only to the callback. */
export type PermissionRequest = {
    /** 稳定 Interaction id（同时是 Interaction 记录的 Map key 与 ToolCall.permissionId）。 */
    interactionId: string;
    sessionId: string;
    toolCallId: string;
    title: string;
    input: unknown;
    options: Array<{ optionId: string; name: string }>;
    signal: AbortSignal;
};

export type ElicitationRequest = {
    interactionId: string;
    sessionId: string;
    message: string;
    requestedSchema: Record<string, unknown>;
    signal: AbortSignal;
};

export type ElicitationDecision =
    | { action: "accept"; content: Record<string, unknown> }
    | { action: "decline" | "cancel" };

/** 反向交互回调：宿主可只提供这两个回调（不需要完整 AgentOptions）。 */
export type InteractionCallbacks = Pick<AgentOptions, "onPermissionRequest" | "onElicitation">;

export interface AgentOptions {
    id: string;
    sandbox: Sandbox;
    /** Required for a new Session; loaded Sessions use their persisted cwd. */
    path?: string;
    instructions?: string;
    mcpServers?: Record<string, AcpMcpServer>;
    onPermissionRequest?: (request: PermissionRequest) => "allow_once" | "reject_once" | Promise<"allow_once" | "reject_once">;
    onElicitation?: (request: ElicitationRequest) => ElicitationDecision | Promise<ElicitationDecision>;
}
