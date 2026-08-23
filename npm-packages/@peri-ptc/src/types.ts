export interface ToolCallParams {
  invocationId: string;
  toolName: string;
  input: unknown;
}

export interface ToolCancelParams {
  invocationId: string;
}

export type ToolCallErrorCode =
  | "UNKNOWN_TOOL"
  | "INVALID_INPUT"
  | "PERMISSION_DENIED"
  | "USER_REJECTED"
  | "CANCELLED"
  | "TIMEOUT"
  | "TOOL_FAILED";

export interface ToolCallOptions {
  signal?: AbortSignal;
}

export type Tool = (
  input?: unknown,
  options?: ToolCallOptions,
) => Promise<unknown>;

export type Tools = Record<string, Tool>;

export interface PtcExecutionResult {
  value: unknown;
  logs: string[];
}
