import { randomBytes } from "node:crypto";
import { DEFAULT_SANDBOX_MODULE, loadSandboxBackend } from "./backend";
import {
  DEFAULT_SANDBOX_SERVER_NAME,
  DEFAULT_STARTUP_TIMEOUT_MS,
  DEFAULT_STOP_TIMEOUT_MS,
  SandboxHandle,
  assertServerName,
  positiveTimeout,
  type SandboxHandleConfig,
} from "./handle";
import {
  SandboxManagerError,
  type SandboxBackend,
  type SandboxManagerOptions,
  type SandboxManagerStartOptions,
  type SandboxProfile,
} from "./types";

interface NormalizedOptions {
  sandboxesRoot?: string;
  settingsPath?: string;
  profileName?: string;
  profile?: SandboxProfile;
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

/** Generate an id accepted by `peri-sandbox` (`sbx_<32 hex>`). */
export function createSandboxId(): string {
  return `sbx_${randomBytes(16).toString("hex")}`;
}

/**
 * Resource registry for in-process sandboxes, keyed by `sandboxId`.
 *
 * The manager knows nothing about chat sessions: it neither receives a
 * session key nor stores a session mapping. Callers decide which session
 * binds to which handle, and {@link SandboxHandle.mcpServer} merely exposes
 * a ready sandbox as data; session init is assembled by the caller.
 */
export class SandboxManager {
  private readonly options: NormalizedOptions;
  private readonly handles = new Map<string, SandboxHandle>();
  private backendValue: SandboxBackend | null = null;

  constructor(options: SandboxManagerOptions = {}) {
    const serverName = options.serverName ?? DEFAULT_SANDBOX_SERVER_NAME;
    assertServerName(serverName);
    this.options = {
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
  }

  /** Number of handles currently owned (closed handles are removed). */
  get size(): number {
    return this.handles.size;
  }

  /** Look up a live handle; closed handles are not returned. */
  get(sandboxId: string): SandboxHandle | undefined {
    return this.handles.get(sandboxId);
  }

  /** Snapshot of the current handles in creation order. */
  list(): SandboxHandle[] {
    return [...this.handles.values()];
  }

  /**
   * Start a sandbox and return its handle.
   *
   * The same `sandboxId` is idempotent: an existing handle is started and
   * returned again. A failed creation removes the half-built handle and
   * rethrows. Without `sandboxId` the manager generates a fresh id, so each
   * such call starts a distinct sandbox.
   */
  async start(options: SandboxManagerStartOptions = {}): Promise<SandboxHandle> {
    const sandboxId = options.sandboxId ?? createSandboxId();
    const existing = this.handles.get(sandboxId);
    if (existing) {
      await existing.start();
      return existing;
    }

    const handle = this.createHandle(sandboxId, options);
    this.handles.set(sandboxId, handle);
    try {
      await handle.start();
      return handle;
    } catch (error) {
      if (this.handles.get(sandboxId) === handle) this.handles.delete(sandboxId);
      throw error;
    }
  }

  /**
   * Close every current handle concurrently.
   *
   * Successful handles detach from the manager; failed ones stay for a retry.
   * Any failure is reported as an `AggregateError` carrying every rejection.
   */
  async closeAll(options: { deleteData?: boolean } = {}): Promise<void> {
    const handles = [...this.handles.values()];
    const results = await Promise.allSettled(handles.map((handle) => handle.close(options)));
    const failures: unknown[] = [];
    for (const result of results) if (result.status === "rejected") failures.push(result.reason);
    if (failures.length > 0)
      throw new AggregateError(
        failures,
        `Sandbox close failed for ${failures.length} of ${handles.length} handle(s).`,
      );
  }

  private createHandle(sandboxId: string, options: SandboxManagerStartOptions): SandboxHandle {
    const serverName = options.serverName ?? this.options.serverName;
    assertServerName(serverName);
    const profileName = options.profileName ?? this.options.profileName;
    const profile = options.profile ?? this.options.profile;
    const port = options.port ?? this.options.port;
    const bashCalls = options.bashCalls ?? this.options.bashCalls;
    const signal = options.signal ?? this.options.signal;
    const config: SandboxHandleConfig = {
      sandboxId,
      serverName,
      ...(this.options.sandboxesRoot === undefined ? {} : { sandboxesRoot: this.options.sandboxesRoot }),
      ...(this.options.settingsPath === undefined ? {} : { settingsPath: this.options.settingsPath }),
      ...(profileName === undefined ? {} : { profileName }),
      ...(profile === undefined ? {} : { profile }),
      ...(port === undefined ? {} : { port }),
      ...(this.options.periPath === undefined ? {} : { periPath: this.options.periPath }),
      ...(this.options.periSha256 === undefined ? {} : { periSha256: this.options.periSha256 }),
      ...(bashCalls === undefined ? {} : { bashCalls }),
      ...(this.options.minFreeBytes === undefined ? {} : { minFreeBytes: this.options.minFreeBytes }),
      startupTimeoutMs: this.options.startupTimeoutMs,
      stopTimeoutMs: this.options.stopTimeoutMs,
      ...(signal === undefined ? {} : { signal }),
    };
    return new SandboxHandle(config, {
      getBackend: () => this.getBackend(),
      onClosed: (handle) => {
        if (this.handles.get(handle.sandboxId) === handle) this.handles.delete(handle.sandboxId);
      },
    });
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
}
