import { isAbsolute } from "node:path";
import { DeadlineError, withDeadline } from "../util/deadline";
import {
  SandboxManagerError,
  type SandboxAdapter,
  type SandboxAdapterInstance,
  type SandboxAdapterStartOptions,
  type SandboxEndpoint,
  type SandboxHandleState,
  type SandboxManagerErrorCode,
  type SandboxMcpHttpConfig,
  type SandboxMcpServer,
  type SandboxStatus,
} from "./types";

export const DEFAULT_SANDBOX_SERVER_NAME = "workspace";
export const DEFAULT_STARTUP_TIMEOUT_MS = 45_000;
export const DEFAULT_STOP_TIMEOUT_MS = 15_000;

const RESERVED_SERVER_NAMES = new Set(["web", "artifact", "cron", "workflow"]);

/** @internal Manager-wide defaults resolved into one handle. */
export interface SandboxHandleConfig<TOptions = unknown> {
  sandboxId: string;
  serverName: string;
  adapterOptions?: TOptions;
  startupTimeoutMs: number;
  stopTimeoutMs: number;
  signal?: AbortSignal;
}

/** @internal Collaborators injected by {@link SandboxManager}. */
export interface SandboxHandleDeps<TOptions = unknown> {
  getAdapter(): SandboxAdapter<TOptions>;
  onClosed?(handle: SandboxHandle<TOptions>): void;
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

function adapterCode(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null) return undefined;
  const code = (error as { code?: unknown }).code;
  return typeof code === "string" ? code : undefined;
}

function mapAdapterCode(code: string | undefined, operation: ErrorOperation): SandboxManagerErrorCode | null {
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
    case "delete_incomplete":
      return "delete_incomplete";
    case "limit_exceeded":
      return "limit_exceeded";
    case "closed":
      return "closed";
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

function validateMcpServer(value: unknown): SandboxMcpHttpConfig {
  if (typeof value !== "object" || value === null || (value as { type?: unknown }).type !== "http")
    throw new SandboxManagerError("start_failed", 'Sandbox adapter must return an HTTP MCP descriptor (type: "http").');
  const { url, headers } = value as { url?: unknown; headers?: unknown };
  if (typeof url !== "string") throw new SandboxManagerError("start_failed", "Sandbox MCP descriptor has no URL.");
  if (typeof headers !== "object" || headers === null || Array.isArray(headers))
    throw new SandboxManagerError("start_failed", "Sandbox MCP descriptor has no headers object.");
  const copy: Record<string, string> = {};
  for (const [name, header] of Object.entries(headers)) {
    if (typeof header !== "string")
      throw new SandboxManagerError("start_failed", `Sandbox MCP header "${name}" must be a string.`);
    copy[name] = header;
  }
  return { type: "http", url: validateMcpUrl(url), headers: copy };
}

function authorizationToken(config: SandboxMcpHttpConfig | null): string | undefined {
  if (!config) return undefined;
  const header = config.headers.Authorization;
  if (!header) return undefined;
  return header.replace(/^Bearer\s+/i, "");
}

/**
 * One sandbox resource owned by a {@link SandboxManager}.
 *
 * The handle owns the instance lifecycle but never a session: binding a
 * ready sandbox to a chat session is the caller's decision. The bearer token
 * leaves only through `mcpServer()`; `status()`, `endpoint()` and errors are
 * redacted.
 */
export class SandboxHandle<TOptions = unknown> {
  readonly sandboxId: string;
  readonly serverName: string;

  private readonly config: SandboxHandleConfig<TOptions>;
  private readonly deps: SandboxHandleDeps<TOptions>;
  private stateValue: SandboxHandleState = "idle";
  private startPromise: Promise<SandboxEndpoint> | null = null;
  private tail: Promise<unknown> = Promise.resolve();
  private closeRequested = false;
  private instanceValue: SandboxAdapterInstance | null = null;
  private mcpConfigValue: SandboxMcpHttpConfig | null = null;
  private endpointValue: SandboxEndpoint | null = null;
  private workspacePathValue: string | null = null;
  private lastErrorValue: { code: SandboxManagerErrorCode; message: string } | null = null;
  private dataDeleted = false;
  private knownToken: string | null = null;
  private closedNotified = false;

  /** @internal Create handles through {@link SandboxManager.start}. */
  constructor(config: SandboxHandleConfig<TOptions>, deps: SandboxHandleDeps<TOptions>) {
    assertServerName(config.serverName);
    this.config = config;
    this.deps = deps;
    this.sandboxId = config.sandboxId;
    this.serverName = config.serverName;
  }

  get state(): SandboxHandleState {
    return this.stateValue;
  }

  /** Workspace path reported by the adapter, or `null` before the first start. */
  get workspacePath(): string | null {
    return this.endpointValue?.workspacePath ?? this.workspacePathValue;
  }

  /**
   * Start or reuse this sandbox instance.
   *
   * `signal` cancels this start only; without one the manager-level default is
   * used. Concurrent calls share one in-flight promise; failures are
   * retryable; calls after `close()` are rejected.
   */
  start(signal?: AbortSignal): Promise<SandboxEndpoint> {
    if (this.closeRequested)
      return Promise.reject(new SandboxManagerError("closed", "Sandbox handle is closed."));
    const ready = this.endpointValue;
    if (this.stateValue === "ready" && ready) return Promise.resolve(ready);
    if (this.startPromise) return this.startPromise;

    const operation = this.enqueue(() => this.performStart(signal ?? this.config.signal));
    this.startPromise = operation;
    const clear = (): void => {
      if (this.startPromise === operation) this.startPromise = null;
    };
    operation.then(clear, clear);
    return operation;
  }

  /** Local snapshot; reads the current instance state and never includes the token. */
  status(): SandboxStatus {
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
    if (this.stateValue === "closed" || this.closeRequested)
      throw new SandboxManagerError("closed", "Sandbox handle is closed.");
    const instance = this.instanceValue;
    const config = this.mcpConfigValue;
    if (this.stateValue !== "ready" || !this.endpointValue || !instance || !config)
      throw new SandboxManagerError("not_ready", `Sandbox is not ready (state: ${this.stateValue}).`);
    return {
      name: this.serverName,
      config: { type: "http", url: config.url, headers: { ...config.headers } },
    };
  }

  /** Close the instance but keep the handle and workspace for a later start. */
  stop(): Promise<void> {
    return this.enqueue(async () => {
      if (this.stateValue === "closed" || this.stateValue === "idle" || this.stateValue === "stopped") return;
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
        this.mcpConfigValue = null;
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
   * The adapter instance is closed first; `deleteData: true` then asks the
   * adapter to delete persisted data when it supports deletion. Idempotent;
   * after this call `start()` rejects with `closed`. A successful close
   * detaches the handle from its manager, while a failed close keeps it for a
   * retry.
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

  private async performStart(signal?: AbortSignal): Promise<SandboxEndpoint> {
    this.stateValue = "starting";
    this.endpointValue = null;
    this.lastErrorValue = null;

    let instance: SandboxAdapterInstance | null = null;
    try {
      const adapter = this.deps.getAdapter();
      const pending = adapter.start(this.startOptions(signal));
      try {
        instance = await withDeadline(pending, this.config.startupTimeoutMs, "Sandbox start");
      } catch (error) {
        void pending.then((late) => late.close()).catch(() => undefined);
        throw error;
      }
      if (typeof instance.close !== "function") {
        throw new SandboxManagerError("start_failed", "Sandbox adapter returned an instance without close().");
      }
      if (this.closeRequested) {
        await instance.close().catch(() => undefined);
        instance = null;
        throw new SandboxManagerError("closed", "Sandbox handle is closed.");
      }
      if (instance.sandboxId !== this.sandboxId) {
        const startedId = instance.sandboxId;
        await instance.close().catch(() => undefined);
        instance = null;
        throw new SandboxManagerError(
          "conflict",
          `Sandbox adapter started "${startedId}" for requested id "${this.sandboxId}".`,
        );
      }
      const workspacePath = validateWorkspace(instance.workspacePath);
      const mcpConfig = validateMcpServer(instance.mcpServer);
      const endpoint: SandboxEndpoint = {
        sandboxId: this.sandboxId,
        serverName: this.serverName,
        mcpUrl: mcpConfig.url,
        workspacePath,
      };
      this.instanceValue = instance;
      this.mcpConfigValue = mcpConfig;
      this.knownToken = authorizationToken(mcpConfig) ?? null;
      this.workspacePathValue = endpoint.workspacePath;
      this.endpointValue = endpoint;
      this.lastErrorValue = null;
      this.stateValue = "ready";
      return endpoint;
    } catch (error) {
      if (instance && this.instanceValue !== instance && typeof instance.close === "function")
        await instance.close().catch(() => undefined);
      this.mcpConfigValue = null;
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
        await withDeadline(instance.close(), this.config.stopTimeoutMs, "Sandbox close");
        this.instanceValue = null;
        this.mcpConfigValue = null;
      }
      if (deleteData) await this.deleteData();
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
      const adapter = this.deps.getAdapter();
      if (!adapter.deleteData) {
        this.dataDeleted = true;
        return;
      }
      await withDeadline(adapter.deleteData(this.sandboxId), this.config.stopTimeoutMs, "Sandbox data delete");
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

  private startOptions(signal?: AbortSignal): SandboxAdapterStartOptions<TOptions> {
    const options: SandboxAdapterStartOptions<TOptions> = {
      sandboxId: this.sandboxId,
      options: this.config.adapterOptions,
    };
    if (signal !== undefined) options.signal = signal;
    return options;
  }

  private mapError(error: unknown, operation: ErrorOperation): SandboxManagerError {
    if (error instanceof SandboxManagerError) return this.redactError(error);
    const token = this.knownToken ?? authorizationToken(this.mcpConfigValue) ?? undefined;
    if (error instanceof DeadlineError) {
      const code: SandboxManagerErrorCode = operation === "stop" ? "stop_incomplete" : "start_failed";
      return new SandboxManagerError(code, error.message);
    }
    const code =
      mapAdapterCode(adapterCode(error), operation) ?? (operation === "stop" ? "stop_incomplete" : "start_failed");
    const message = error instanceof Error ? error.message : String(error);
    const redacted = this.redact(message, token);
    return new SandboxManagerError(code, redacted, {
      ...(token && message.includes(token) ? {} : { cause: error }),
    });
  }

  private redactError(error: SandboxManagerError): SandboxManagerError {
    const token = this.knownToken ?? authorizationToken(this.mcpConfigValue) ?? undefined;
    const message = this.redact(error.message, token);
    if (message === error.message) return error;
    return new SandboxManagerError(error.code, message);
  }

  private redact(message: string, token?: string): string {
    let text = message;
    if (token) text = text.split(token).join("[redacted]");
    return text.replace(/Bearer\s+[^\s"']+/gi, "Bearer [redacted]");
  }
}
