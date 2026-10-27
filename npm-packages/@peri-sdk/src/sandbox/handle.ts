import { isAbsolute } from "node:path";
import { DeadlineError, withDeadline } from "../util/deadline";
import {
  SandboxManagerError,
  type SandboxBackend,
  type SandboxBackendInstance,
  type SandboxBackendStartOptions,
  type SandboxEndpoint,
  type SandboxHandleState,
  type SandboxManagerErrorCode,
  type SandboxMcpServer,
  type SandboxProfile,
  type SandboxStatus,
} from "./types";

export const DEFAULT_SANDBOX_SERVER_NAME = "workspace";
export const DEFAULT_STARTUP_TIMEOUT_MS = 45_000;
export const DEFAULT_STOP_TIMEOUT_MS = 15_000;

const RESERVED_SERVER_NAMES = new Set(["web", "artifact", "cron", "workflow"]);

/** @internal Manager-wide defaults resolved into one handle. */
export interface SandboxHandleConfig {
  sandboxId: string;
  serverName: string;
  sandboxesRoot?: string;
  settingsPath?: string;
  profileName?: string;
  profile?: SandboxProfile;
  port?: number;
  periPath?: string;
  periSha256?: string;
  bashCalls?: number;
  minFreeBytes?: number;
  startupTimeoutMs: number;
  stopTimeoutMs: number;
  signal?: AbortSignal;
}

/** @internal Collaborators injected by {@link SandboxManager}. */
export interface SandboxHandleDeps {
  getBackend(): Promise<SandboxBackend>;
  onClosed?(handle: SandboxHandle): void;
}

type ErrorOperation = "start" | "stop";

export function positiveTimeout(value: number | undefined, fallback: number): number {
  return value !== undefined && Number.isFinite(value) && value > 0 ? value : fallback;
}

export function assertServerName(name: string): void {
  if (name.trim().length === 0)
    throw new SandboxManagerError("invalid_server_name", "Sandbox MCP server name must not be empty.");
  if (RESERVED_SERVER_NAMES.has(name))
    throw new SandboxManagerError(
      "invalid_server_name",
      `Sandbox MCP server name "${name}" is reserved by Peri; use "workspace" or another name.`,
    );
}

function backendCode(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null) return undefined;
  const code = (error as { code?: unknown }).code;
  return typeof code === "string" ? code : undefined;
}

function mapBackendCode(code: string | undefined, operation: ErrorOperation): SandboxManagerErrorCode | null {
  switch (code) {
    case "unsupported_environment":
      return "unsupported_environment";
    case "invalid_options":
      return "invalid_options";
    case "conflict":
      return "conflict";
    case "start_failed":
      return "start_failed";
    case "start_cancelled":
      return "cancelled";
    case "stop_incomplete":
      return "stop_incomplete";
    case "backend_exited":
      return "backend_exited";
    case "delete_incomplete":
      return "delete_incomplete";
    case "limit_exceeded":
      return "limit_exceeded";
    case "closed":
      return "closed";
    case "token_unreadable":
      return "token_unavailable";
    default:
      return operation === "stop" ? "stop_incomplete" : null;
  }
}

function validateMcpUrl(value: string): string {
  let url: URL;
  try {
    url = new URL(value);
  } catch (error) {
    throw new SandboxManagerError("unsupported_environment", "Sandbox MCP URL is not a valid absolute URL.", {
      cause: error,
    });
  }
  if (url.protocol !== "http:" && url.protocol !== "https:")
    throw new SandboxManagerError("unsupported_environment", "Sandbox MCP URL must use http or https.");
  if (url.username.length > 0 || url.password.length > 0)
    throw new SandboxManagerError("unsupported_environment", "Sandbox MCP URL must not embed credentials.");
  const host = url.hostname;
  if (host !== "127.0.0.1" && host !== "localhost" && host !== "::1" && host !== "[::1]")
    throw new SandboxManagerError("unsupported_environment", "Sandbox MCP URL must point to a loopback host.");
  return value;
}

function validateWorkspace(value: string): string {
  if (!isAbsolute(value))
    throw new SandboxManagerError("workspace_unavailable", "Sandbox workspace path must be an absolute host path.");
  return value;
}

/**
 * One sandbox resource owned by a {@link SandboxManager}.
 *
 * The handle owns the instance lifecycle but never a session: binding a
 * ready sandbox to a chat session is the caller's decision. The bearer token
 * leaves only through `mcpServer()`; `status()`, `endpoint()` and errors are
 * redacted.
 */
export class SandboxHandle {
  readonly sandboxId: string;
  readonly serverName: string;

  private readonly config: SandboxHandleConfig;
  private readonly deps: SandboxHandleDeps;
  private stateValue: SandboxHandleState = "idle";
  private startPromise: Promise<SandboxEndpoint> | null = null;
  private tail: Promise<unknown> = Promise.resolve();
  private closeRequested = false;
  private instanceValue: SandboxBackendInstance | null = null;
  private endpointValue: SandboxEndpoint | null = null;
  private workspacePathValue: string | null = null;
  private lastErrorValue: { code: SandboxManagerErrorCode; message: string } | null = null;
  private dataDeleted = false;
  private knownToken: string | null = null;
  private closedNotified = false;

  /** @internal Create handles through {@link SandboxManager.start}. */
  constructor(config: SandboxHandleConfig, deps: SandboxHandleDeps) {
    assertServerName(config.serverName);
    this.config = config;
    this.deps = deps;
    this.sandboxId = config.sandboxId;
    this.serverName = config.serverName;
  }

  get state(): SandboxHandleState {
    this.syncInstanceState();
    return this.stateValue;
  }

  /** Workspace path reported by the backend, or `null` before the first start. */
  get workspacePath(): string | null {
    this.syncInstanceState();
    return this.endpointValue?.workspacePath ?? this.workspacePathValue;
  }

  /**
   * Start or reuse this sandbox instance.
   *
   * Concurrent calls share one in-flight promise; failures are retryable;
   * calls after `close()` are rejected.
   */
  start(): Promise<SandboxEndpoint> {
    if (this.closeRequested)
      return Promise.reject(new SandboxManagerError("closed", "Sandbox handle is closed."));
    this.syncInstanceState();
    const ready = this.endpointValue;
    if (this.stateValue === "ready" && ready) return Promise.resolve(ready);
    if (this.startPromise) return this.startPromise;

    const operation = this.enqueue(() => this.performStart());
    this.startPromise = operation;
    const clear = (): void => {
      if (this.startPromise === operation) this.startPromise = null;
    };
    operation.then(clear, clear);
    return operation;
  }

  /** Local snapshot; reads the current instance state and never includes the token. */
  status(): SandboxStatus {
    this.syncInstanceState();
    return {
      sandboxId: this.sandboxId,
      state: this.stateValue,
      mcpUrl: this.endpointValue?.mcpUrl ?? null,
      workspacePath: this.endpointValue?.workspacePath ?? this.workspacePathValue,
      lastError: this.lastErrorValue ? { ...this.lastErrorValue } : null,
    };
  }

  /** Return the current endpoint; never includes the bearer token. */
  endpoint(): SandboxEndpoint | null {
    this.syncInstanceState();
    return this.endpointValue;
  }

  /**
   * Project a ready sandbox into one HTTP MCP server entry.
   *
   * Rejects with `not_ready` unless the instance is ready and with `closed`
   * after `close()`. This is the only place the bearer token leaves the
   * handle; callers assemble their own session init from this value.
   */
  async mcpServer(): Promise<SandboxMcpServer> {
    this.syncInstanceState();
    if (this.stateValue === "closed" || this.closeRequested)
      throw new SandboxManagerError("closed", "Sandbox handle is closed.");
    const endpoint = this.endpointValue;
    const instance = this.instanceValue;
    if (this.stateValue !== "ready" || !endpoint || !instance)
      throw new SandboxManagerError("not_ready", `Sandbox is not ready (state: ${this.stateValue}).`);
    if (!instance.token)
      throw new SandboxManagerError("token_unavailable", "Sandbox instance did not provide a token.");
    return {
      name: this.serverName,
      config: {
        type: "http",
        url: endpoint.mcpUrl,
        headers: { Authorization: `Bearer ${instance.token}` },
      },
    };
  }

  /** Close the instance but keep the handle and workspace for a later start. */
  stop(): Promise<void> {
    return this.enqueue(async () => {
      if (this.stateValue === "closed" || this.stateValue === "idle" || this.stateValue === "stopped") return;
      this.syncInstanceState();
      const instance = this.instanceValue;
      if (!instance) {
        this.endpointValue = null;
        this.stateValue = "stopped";
        return;
      }
      this.stateValue = "stopping";
      try {
        await withDeadline(instance.close(), this.config.stopTimeoutMs, "Sandbox stop");
        this.instanceValue = null;
        this.endpointValue = null;
        this.lastErrorValue = null;
        this.stateValue = "stopped";
      } catch (error) {
        const mapped = this.mapError(error, "stop");
        this.endpointValue = null;
        this.lastErrorValue = { code: mapped.code, message: mapped.message };
        this.stateValue = "failed";
        throw mapped;
      }
    });
  }

  /**
   * Terminal close.
   *
   * `deleteData: true` also removes the sandbox directory. Idempotent; after
   * this call `start()` rejects with `closed`. A successful close detaches the
   * handle from its manager, while a failed close keeps it for a retry.
   */
  close(options: { deleteData?: boolean } = {}): Promise<void> {
    const deleteData = options.deleteData === true;
    if (this.stateValue === "closed") {
      if (deleteData && !this.dataDeleted) return this.enqueue(() => this.deleteData());
      return Promise.resolve();
    }
    this.closeRequested = true;
    return this.enqueue(() => this.performClose(deleteData));
  }

  private enqueue<T>(operation: () => Promise<T>): Promise<T> {
    const next = this.tail.then(operation, operation);
    this.tail = next.then(
      () => undefined,
      () => undefined,
    );
    return next;
  }

  private syncInstanceState(): void {
    if (
      this.instanceValue &&
      this.instanceValue.state === "failed" &&
      (this.stateValue === "ready" || this.stateValue === "starting")
    ) {
      this.stateValue = "failed";
      this.endpointValue = null;
      this.lastErrorValue = {
        code: "backend_exited",
        message: "Sandbox backend exited unexpectedly.",
      };
    }
  }

  private async performStart(): Promise<SandboxEndpoint> {
    this.syncInstanceState();
    this.stateValue = "starting";
    this.endpointValue = null;
    this.lastErrorValue = null;

    let instance: SandboxBackendInstance | null = null;
    try {
      const backend = await this.deps.getBackend();
      const pending = backend.start(this.startOptions());
      try {
        instance = await withDeadline(pending, this.config.startupTimeoutMs, "Sandbox start");
      } catch (error) {
        void pending.then((late) => late.close()).catch(() => undefined);
        throw error;
      }
      if (this.closeRequested) {
        await instance.close().catch(() => undefined);
        throw new SandboxManagerError("closed", "Sandbox handle is closed.");
      }
      if (instance.state !== "ready") {
        const state = instance.state;
        await instance.close().catch(() => undefined);
        throw new SandboxManagerError("start_failed", `Sandbox did not become ready (state: ${state}).`);
      }
      if (instance.id !== this.sandboxId) {
        await instance.close().catch(() => undefined);
        throw new SandboxManagerError(
          "conflict",
          `Sandbox backend started "${instance.id}" for requested id "${this.sandboxId}".`,
        );
      }
      const endpoint: SandboxEndpoint = {
        sandboxId: this.sandboxId,
        serverName: this.serverName,
        mcpUrl: validateMcpUrl(instance.mcpUrl),
        runId: instance.runId,
        workspacePath: validateWorkspace(instance.workspacePath),
      };
      this.instanceValue = instance;
      this.knownToken = instance.token;
      this.workspacePathValue = endpoint.workspacePath;
      this.endpointValue = endpoint;
      this.lastErrorValue = null;
      this.stateValue = "ready";
      return endpoint;
    } catch (error) {
      if (instance && this.instanceValue !== instance) await instance.close().catch(() => undefined);
      const mapped = this.mapError(error, "start");
      this.stateValue = "failed";
      this.lastErrorValue = { code: mapped.code, message: mapped.message };
      this.endpointValue = null;
      throw mapped;
    }
  }

  private async performClose(deleteData: boolean): Promise<void> {
    if (this.stateValue === "closed") {
      if (deleteData && !this.dataDeleted) await this.deleteData();
      return;
    }
    this.stateValue = "stopping";
    const instance = this.instanceValue;
    try {
      if (instance) {
        await withDeadline(
          instance.close(deleteData ? { deleteData: true } : undefined),
          this.config.stopTimeoutMs,
          "Sandbox close",
        );
        this.instanceValue = null;
        if (deleteData) this.dataDeleted = true;
      } else if (deleteData && this.sandboxId) {
        await this.deleteData();
      }
      this.endpointValue = null;
      this.lastErrorValue = null;
      this.stateValue = "closed";
      this.notifyClosed();
    } catch (error) {
      const mapped = this.mapError(error, "stop");
      this.stateValue = "failed";
      this.lastErrorValue = { code: mapped.code, message: mapped.message };
      throw mapped;
    }
  }

  private async deleteData(): Promise<void> {
    try {
      const backend = await this.deps.getBackend();
      await withDeadline(
        backend.deleteData({
          sandboxId: this.sandboxId,
          ...(this.config.sandboxesRoot === undefined ? {} : { sandboxesRoot: this.config.sandboxesRoot }),
          ...(this.config.settingsPath === undefined ? {} : { settingsPath: this.config.settingsPath }),
        }),
        this.config.stopTimeoutMs,
        "Sandbox data delete",
      );
      this.dataDeleted = true;
    } catch (error) {
      const mapped = this.mapError(error, "stop");
      this.lastErrorValue = { code: mapped.code, message: mapped.message };
      if (mapped.code === "delete_incomplete") throw mapped;
      throw new SandboxManagerError("delete_incomplete", mapped.message, { cause: mapped });
    }
  }

  private notifyClosed(): void {
    if (this.closedNotified) return;
    this.closedNotified = true;
    this.deps.onClosed?.(this);
  }

  private startOptions(): SandboxBackendStartOptions {
    const options = this.config;
    return {
      sandboxId: options.sandboxId,
      ...(options.sandboxesRoot === undefined ? {} : { sandboxesRoot: options.sandboxesRoot }),
      ...(options.settingsPath === undefined ? {} : { settingsPath: options.settingsPath }),
      ...(options.profileName === undefined ? {} : { profileName: options.profileName }),
      ...(options.profile === undefined ? {} : { profile: options.profile }),
      ...(options.periPath === undefined ? {} : { periPath: options.periPath }),
      ...(options.periSha256 === undefined ? {} : { periSha256: options.periSha256 }),
      ...(options.bashCalls === undefined ? {} : { bashCalls: options.bashCalls }),
      ...(options.minFreeBytes === undefined ? {} : { minFreeBytes: options.minFreeBytes }),
      ...(options.port === undefined ? {} : { port: options.port }),
      startupTimeoutMs: options.startupTimeoutMs,
      ...(options.signal === undefined ? {} : { signal: options.signal }),
    };
  }

  private mapError(error: unknown, operation: ErrorOperation): SandboxManagerError {
    if (error instanceof SandboxManagerError) return error;
    const token = this.instanceValue?.token ?? this.knownToken ?? undefined;
    if (error instanceof DeadlineError) {
      const code: SandboxManagerErrorCode = operation === "stop" ? "stop_incomplete" : "start_failed";
      return new SandboxManagerError(code, error.message);
    }
    const code =
      mapBackendCode(backendCode(error), operation) ?? (operation === "stop" ? "stop_incomplete" : "start_failed");
    const message = error instanceof Error ? error.message : String(error);
    const redacted = this.redact(message || `Sandbox ${operation} failed.`, token);
    return new SandboxManagerError(code, redacted, {
      ...(token && message.includes(token) ? {} : { cause: error }),
    });
  }

  private redact(message: string, token?: string): string {
    let text = message;
    if (token) text = text.split(token).join("[redacted]");
    return text.replace(/Bearer\s+[^\s"']+/gi, "Bearer [redacted]");
  }
}
