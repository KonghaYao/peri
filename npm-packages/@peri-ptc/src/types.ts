export const PTC_PROTOCOL_VERSION = 1 as const;
export const PTC_START_METHOD = "ptc/start" as const;
export const PTC_EXECUTE_METHOD = "execute" as const;

export interface PtcStartParams {
  protocolVersion: typeof PTC_PROTOCOL_VERSION;
}

export interface PtcStartResult {
  protocolVersion: typeof PTC_PROTOCOL_VERSION;
  buildId: string;
}

export interface PtcExecutionLimits {
  maxFrameBytes?: number;
  maxLogsBytes?: number;
  maxResultBytes?: number;
}

export interface PtcExecuteParams {
  source: string;
  input: unknown;
  limits?: PtcExecutionLimits;
}

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
  | "TOOL_FAILED"
  | "RESOURCE_LIMIT";

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
