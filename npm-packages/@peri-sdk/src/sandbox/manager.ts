import { isAbsolute } from "node:path";
import { DeadlineError, withDeadline } from "../util/deadline";
import { DEFAULT_SANDBOX_MODULE, loadSandboxBackend } from "./backend";
import {
  SandboxManagerError,
  type SandboxBackend,
  type SandboxBackendInstance,
  type SandboxBackendStartOptions,
  type SandboxChatSessionInit,
  type SandboxEndpoint,
  type SandboxManagerErrorCode,
  type SandboxManagerOptions,
  type SandboxManagerState,
  type SandboxStatus,
} from "./types";

const DEFAULT_SERVER_NAME = "workspace";
const RESERVED_SERVER_NAMES = new Set(["web", "artifact", "cron", "workflow"]);
const DEFAULT_STARTUP_TIMEOUT_MS = 45_000;
const DEFAULT_STOP_TIMEOUT_MS = 15_000;

interface NormalizedOptions {
  sandboxId?: string;
  sessionKey?: string;
  sandboxesRoot?: string;
  settingsPath?: string;
  profileName?: string;
  profile?: SandboxManagerOptions["profile"];
  port?: number;
  periPath?: string;
  periSha256?: string;
  bashCalls?: number;
  minFreeBytes?: number;
  serverName: string;
  moduleSpecifier: string;
  startupTimeoutMs: number;
  stopTimeoutMs: number;
  backend?: SandboxBackend | (() => Promise<SandboxBackend>);
  signal?: AbortSignal;
}

type ErrorOperation = "start" | "stop";

function positiveTimeout(value: number | undefined, fallback: number): number {
  return value !== undefined && Number.isFinite(value) && value > 0 ? value : fallback;
}

function assertServerName(name: string): void {
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
 * Owns one in-process sandbox instance and exposes the HTTP MCP descriptor
 * that `ChatSessionInit.mcpServers` consumes.
 *
 * Create one manager per chat session. The manager never creates an ACP
 * transport, never touches a Chat, and never returns the bearer token outside
 * `chatSessionInit()`.
 */
export class SandboxManager {
  private readonly options: NormalizedOptions;
  private stateValue: SandboxManagerState = "idle";
  private startPromise: Promise<SandboxEndpoint> | null = null;
  private tail: Promise<unknown> = Promise.resolve();
  private closeRequested = false;
  private backendValue: SandboxBackend | null = null;
  private instanceValue: SandboxBackendInstance | null = null;
  private sandboxIdValue: string | null;
  private endpointValue: SandboxEndpoint | null = null;
  private workspacePathValue: string | null = null;
  private lastErrorValue: { code: SandboxManagerErrorCode; message: string } | null = null;
  private dataDeleted = false;
  private knownToken: string | null = null;

  constructor(options: SandboxManagerOptions = {}) {
    const serverName = options.serverName ?? DEFAULT_SERVER_NAME;
    assertServerName(serverName);
    if (options.sandboxId !== undefined && options.sessionKey !== undefined)
      throw new SandboxManagerError("invalid_options", "Pass either sandboxId or sessionKey, not both.");
    this.options = {
      ...(options.sandboxId === undefined ? {} : { sandboxId: options.sandboxId }),
      ...(options.sessionKey === undefined ? {} : { sessionKey: options.sessionKey }),
      ...(options.sandboxesRoot === undefined ? {} : { sandboxesRoot: options.sandboxesRoot }),
      ...(options.settingsPath === undefined ? {} : { settingsPath: options.settingsPath }),
      ...(options.profileName === undefined ? {} : { profileName: options.profileName }),
      ...(options.profile === undefined ? {} : { profile: options.profile }),
      ...(options.port === undefined ? {} : { port: options.port }),
      ...(options.periPath === undefined ? {} : { periPath: options.periPath }),
      ...(options.periSha256 === undefined ? {} : { periSha256: options.periSha256 }),
      ...(options.bashCalls === undefined ? {} : { bashCalls: options.bashCalls }),
      ...(options.minFreeBytes === undefined ? {} : { minFreeBytes: options.minFreeBytes }),
      serverName,
      moduleSpecifier: options.moduleSpecifier ?? DEFAULT_SANDBOX_MODULE,
      startupTimeoutMs: positiveTimeout(options.startupTimeoutMs, DEFAULT_STARTUP_TIMEOUT_MS),
      stopTimeoutMs: positiveTimeout(options.stopTimeoutMs, DEFAULT_STOP_TIMEOUT_MS),
      ...(options.backend === undefined ? {} : { backend: options.backend }),
      ...(options.signal === undefined ? {} : { signal: options.signal }),
    };
    this.sandboxIdValue = options.sandboxId ?? null;
  }

  get state(): SandboxManagerState {
    this.syncInstanceState();
    return this.stateValue;
  }

  /**
   * Start or reuse the in-process sandbox.
   *
   * Concurrent calls share one in-flight promise; failures are retryable;
   * calls after `close()` are rejected.
   */
  start(): Promise<SandboxEndpoint> {
    if (this.closeRequested) return Promise.reject(new SandboxManagerError("closed", "Sandbox manager is closed."));
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
      managerState: this.stateValue,
      sandboxState: this.instanceValue?.state ?? null,
      sandboxId: this.instanceValue?.id ?? this.sandboxIdValue,
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
   * Ensure the sandbox is ready and build session init input for ChatManager.
   *
   * `path` is the sandbox workspace as seen by the host; `mcpServers` carries
   * the HTTP descriptor with a bearer header. This is the only place the
   * token leaves the manager.
   */
  async chatSessionInit(options: { instructions?: string } = {}): Promise<SandboxChatSessionInit> {
    const endpoint = await this.start();
    const instance = this.instanceValue;
    if (!instance) throw new SandboxManagerError("start_failed", "Sandbox instance disappeared after start.");
    if (!instance.token)
      throw new SandboxManagerError("token_unavailable", "Sandbox instance did not provide a token.");
    return {
      path: endpoint.workspacePath,
      ...(options.instructions === undefined ? {} : { instructions: options.instructions }),
      mcpServers: {
        [this.options.serverName]: {
          type: "http",
          url: endpoint.mcpUrl,
          headers: { Authorization: `Bearer ${instance.token}` },
        },
      },
    };
  }

  /** Close the instance but keep the sandbox id/workspace for a later start. */
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
      this.sandboxIdValue = instance.id;
      try {
        await withDeadline(instance.close(), this.options.stopTimeoutMs, "Sandbox stop");
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
   * this call `start()` rejects with `closed`.
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
      const backend = await this.getBackend();
      const pending = backend.start(this.startOptions());
      try {
        instance = await withDeadline(pending, this.options.startupTimeoutMs, "Sandbox start");
      } catch (error) {
        void pending.then((late) => late.close()).catch(() => undefined);
        throw error;
      }
      if (this.closeRequested) {
        await instance.close().catch(() => undefined);
        throw new SandboxManagerError("closed", "Sandbox manager is closed.");
      }
      if (instance.state !== "ready") {
        const state = instance.state;
        await instance.close().catch(() => undefined);
        throw new SandboxManagerError("start_failed", `Sandbox did not become ready (state: ${state}).`);
      }
      const endpoint: SandboxEndpoint = {
        sandboxId: instance.id,
        serverName: this.options.serverName,
        mcpUrl: validateMcpUrl(instance.mcpUrl),
        runId: instance.runId,
        workspacePath: validateWorkspace(instance.workspacePath),
      };
      this.instanceValue = instance;
      this.sandboxIdValue = instance.id;
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
    if (instance) this.sandboxIdValue = instance.id;
    try {
      if (instance) {
        await withDeadline(
          instance.close(deleteData ? { deleteData: true } : undefined),
          this.options.stopTimeoutMs,
          "Sandbox close",
        );
        this.instanceValue = null;
        if (deleteData) this.dataDeleted = true;
      } else if (deleteData && this.sandboxIdValue) {
        await this.deleteData();
      }
      this.endpointValue = null;
      this.lastErrorValue = null;
      this.stateValue = "closed";
    } catch (error) {
      const mapped = this.mapError(error, "stop");
      this.stateValue = "failed";
      this.lastErrorValue = { code: mapped.code, message: mapped.message };
      throw mapped;
    }
  }

  private async deleteData(): Promise<void> {
    const sandboxId = this.sandboxIdValue ?? this.instanceValue?.id;
    if (!sandboxId)
      throw new SandboxManagerError("delete_incomplete", "Cannot delete sandbox data without a sandbox id.");
    try {
      const backend = await this.getBackend();
      await withDeadline(
        backend.deleteData({
          sandboxId,
          ...(this.options.sandboxesRoot === undefined ? {} : { sandboxesRoot: this.options.sandboxesRoot }),
          ...(this.options.settingsPath === undefined ? {} : { settingsPath: this.options.settingsPath }),
        }),
        this.options.stopTimeoutMs,
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

  private startOptions(): SandboxBackendStartOptions {
    const options = this.options;
    return {
      ...(this.sandboxIdValue === null ? {} : { sandboxId: this.sandboxIdValue }),
      ...(options.sessionKey === undefined ? {} : { sessionKey: options.sessionKey }),
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

  private async getBackend(): Promise<SandboxBackend> {
    if (this.backendValue) return this.backendValue;
    const injected = this.options.backend;
    if (!injected) {
      this.backendValue = loadSandboxBackend(this.options.moduleSpecifier);
      return this.backendValue;
    }
    if (typeof injected !== "function") {
      this.backendValue = injected;
      return injected;
    }
    try {
      const resolved = await injected();
      this.backendValue = resolved;
      return resolved;
    } catch (error) {
      throw new SandboxManagerError("backend_unavailable", "Sandbox backend factory failed.", { cause: error });
    }
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
