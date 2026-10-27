/** Lifecycle states of one in-process sandbox instance. */
export type SandboxState = 'starting' | 'ready' | 'stopping' | 'stopped' | 'failed' | 'closed';

/** Stable error codes exposed by the single-instance API. */
export type SandboxErrorCode =
  | 'unsupported_environment'
  | 'invalid_options'
  | 'token_unreadable'
  | 'conflict'
  | 'start_failed'
  | 'start_cancelled'
  | 'stop_incomplete'
  | 'backend_exited'
  | 'delete_incomplete'
  | 'limit_exceeded'
  | 'closed';

/** Failure raised by the single-instance sandbox API. */
export class SandboxError extends Error {
  readonly code: SandboxErrorCode;

  constructor(code: SandboxErrorCode, message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = 'SandboxError';
    this.code = code;
  }
}

export interface SandboxProfile {
  allowedDomains: string[];
  runtimeReadPaths: string[];
  /** Linux cgroup memory limit, e.g. "1G". */
  memoryMax: string;
  /** Linux pids limit. */
  tasksMax: number;
  /** Linux CPU quota, e.g. "100%". */
  cpuQuota: string;
}

export interface SandboxRuntimeInfo {
  readonly provider: 'macos-process-group' | 'systemd-user-unit';
  /** Transient systemd unit on Linux; null on macOS. */
  readonly unit: string | null;
  /** Launcher/backend pid when known. */
  readonly pid: number | null;
}

export interface SandboxStatus {
  readonly id: string;
  readonly runId: string;
  readonly state: SandboxState;
  readonly mcpUrl: string;
  readonly workspacePath: string;
  readonly sandboxRoot: string;
  readonly runtime: SandboxRuntimeInfo;
  readonly lastError: { code: SandboxErrorCode; message: string } | null;
}

export interface SandboxInstance {
  readonly id: string;
  readonly runId: string;
  readonly state: SandboxState;
  readonly mcpUrl: string;
  /** Sensitive: only for building the MCP Authorization header. */
  readonly token: string;
  readonly tokenFile: string | null;
  readonly workspacePath: string;
  readonly sandboxRoot: string;
  readonly runtime: SandboxRuntimeInfo;
  status(): SandboxStatus;
  close(options?: { deleteData?: boolean }): Promise<void>;
}

export interface StartSandboxOptions {
  /** Complete id: `sbx_[a-f0-9]{32}`. */
  sandboxId?: string;
  /** Alternative to `sandboxId`; derived deterministically. */
  sessionKey?: string;
  sandboxesRoot?: string;
  settingsPath?: string;
  profileName?: string;
  profile?: SandboxProfile;
  /** In-memory token override; mutually exclusive with `tokenFile`. */
  token?: string;
  /** Optional persisted token inside `<sandboxRoot>`; must stay inside it. */
  tokenFile?: string;
  host?: '127.0.0.1' | '::1';
  /** Default 0 (ephemeral). An occupied explicit port is a conflict. */
  port?: number;
  periPath?: string;
  periSha256?: string;
  bashCalls?: number;
  startupTimeoutMs?: number;
  stopTimeoutMs?: number;
  minFreeBytes?: number;
  signal?: AbortSignal;
}

export interface SandboxSettings {
  sandboxesRoot: string;
  periPath: string;
  periSha256: string;
  defaultProfile?: string;
  /** Linux transient unit prefix; defaults to "peri-sbx-". */
  unitPrefix?: string;
  profiles: Record<string, SandboxProfile>;
  startupTimeoutMs?: number;
  stopTimeoutMs?: number;
}

/** Minimal backend surface consumed by the HTTP MCP handler and the instance. */
export interface McpBackendPort {
  readonly runId: string;
  listTools(): unknown;
  forward(
    method: string,
    params: Record<string, unknown>,
    owner: string | undefined,
    signal: AbortSignal,
    onProgress?: (progress: { progress: number; total?: number; message?: string }) => void,
  ): Promise<Record<string, unknown>>;
  close(): Promise<void>;
}
