import type { Sandbox } from "../sandbox/sandbox";

export type AcpMcpServer =
    | { type: "http"; url: string; headers?: Record<string, string> }
    | {
          type: "stdio";
          command: string;
          args?: string[];
          env?: Record<string, string>;
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
