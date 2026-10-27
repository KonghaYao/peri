import type { AcpMcpServer } from "../contracts/acp";

/** Manager lifecycle states exposed by {@link SandboxManager}. */
export type SandboxManagerState = "idle" | "starting" | "ready" | "stopping" | "stopped" | "failed" | "closed";

/** Lifecycle states reported by one in-process sandbox instance. */
export type SandboxInstanceState = "starting" | "ready" | "stopping" | "stopped" | "failed" | "closed";

/** Stable error codes for sandbox lifecycle failures. */
export type SandboxManagerErrorCode =
  | "backend_unavailable"
  | "unsupported_environment"
  | "invalid_options"
  | "invalid_server_name"
  | "conflict"
  | "start_failed"
  | "stop_incomplete"
  | "delete_incomplete"
  | "backend_exited"
  | "workspace_unavailable"
  | "token_unavailable"
  | "limit_exceeded"
  | "closed"
  | "cancelled";

/** Error raised by the sandbox entry; `cause` preserves the backend failure. */
export class SandboxManagerError extends Error {
  readonly code: SandboxManagerErrorCode;

  constructor(code: SandboxManagerErrorCode, message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = "SandboxManagerError";
    this.code = code;
  }
}

export interface SandboxProfile {
  allowedDomains: string[];
  runtimeReadPaths: string[];
  memoryMax: string;
  tasksMax: number;
  cpuQuota: string;
}

/** Options forwarded to `startSandbox()` in the optional backend package. */
export interface SandboxBackendStartOptions {
  sandboxId?: string;
  sessionKey?: string;
  sandboxesRoot?: string;
  settingsPath?: string;
  profileName?: string;
  profile?: SandboxProfile;
  periPath?: string;
  periSha256?: string;
  bashCalls?: number;
  minFreeBytes?: number;
  port?: number;
  startupTimeoutMs?: number;
  signal?: AbortSignal;
}

/** One started sandbox instance as seen through the backend boundary. */
export interface SandboxBackendInstance {
  readonly id: string;
  readonly runId: string;
  readonly state: SandboxInstanceState;
  readonly mcpUrl: string;
  /** Sensitive: only used to build the MCP Authorization header. */
  readonly token: string;
  readonly tokenFile: string | null;
  readonly workspacePath: string;
  readonly sandboxRoot: string;
  close(options?: { deleteData?: boolean }): Promise<void>;
}

/** Backend boundary around the optional `peri-sandbox` package. */
export interface SandboxBackend {
  start(options: SandboxBackendStartOptions): Promise<SandboxBackendInstance>;
  deleteData(options: { sandboxId: string; sandboxesRoot?: string; settingsPath?: string }): Promise<void>;
}

export interface SandboxManagerOptions {
  /** Persist and pass this to reuse the same workspace across restarts. */
  sandboxId?: string;
  /** Alternative to `sandboxId`; derived deterministically by peri-sandbox. */
  sessionKey?: string;
  sandboxesRoot?: string;
  settingsPath?: string;
  profileName?: string;
  profile?: SandboxProfile;
  /** 0 (default) binds an ephemeral port. */
  port?: number;
  periPath?: string;
  periSha256?: string;
  bashCalls?: number;
  minFreeBytes?: number;
  /** ACP MCP server key; defaults to "workspace". */
  serverName?: string;
  /** Module loaded on first use; defaults to "peri-sandbox". */
  moduleSpecifier?: string;
  /** Start budget; defaults to 45_000ms. */
  startupTimeoutMs?: number;
  /** Stop/close budget; defaults to 15_000ms. */
  stopTimeoutMs?: number;
  backend?: SandboxBackend | (() => Promise<SandboxBackend>);
  signal?: AbortSignal;
}

/** Endpoint visible to callers; never contains the bearer token. */
export interface SandboxEndpoint {
  readonly sandboxId: string;
  readonly serverName: string;
  readonly mcpUrl: string;
  readonly runId: string;
  readonly workspacePath: string;
}

export interface SandboxStatus {
  readonly managerState: SandboxManagerState;
  readonly sandboxState: SandboxInstanceState | null;
  readonly sandboxId: string | null;
  readonly mcpUrl: string | null;
  readonly workspacePath: string | null;
  readonly lastError: { code: SandboxManagerErrorCode; message: string } | null;
}

/**
 * Shape accepted by `ChatSessionInit`; kept structural so the sandbox layer
 * does not depend on the chat control plane.
 */
export interface SandboxChatSessionInit {
  path: string;
  instructions?: string;
  mcpServers: Record<string, AcpMcpServer>;
}
