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
    interactionId: number;
    sessionId: string;
    toolCallId: string;
    title: string;
    input: unknown;
    options: Array<{ optionId: string; name: string }>;
    signal: AbortSignal;
};

export type ElicitationRequest = {
    interactionId: number;
    sessionId: string;
    message: string;
    requestedSchema: Record<string, unknown>;
    signal: AbortSignal;
};

export type ElicitationDecision =
    | { action: "accept"; content: Record<string, unknown> }
    | { action: "decline" | "cancel" };

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
