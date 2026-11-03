import { randomBytes } from "node:crypto";
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
  type SandboxAdapter,
  type SandboxManagerOptions,
  type SandboxManagerStartOptions,
} from "./types";

interface NormalizedOptions<TOptions> {
  adapter: SandboxAdapter<TOptions>;
  adapterOptions?: TOptions;
  serverName: string;
  startupTimeoutMs: number;
  stopTimeoutMs: number;
  signal?: AbortSignal;
}

/** Generate a stable sandbox id (`sbx_<32 hex>`). */
export function createSandboxId(): string {
  return `sbx_${randomBytes(16).toString("hex")}`;
}

function assertAdapter<TOptions>(adapter: SandboxAdapter<TOptions> | undefined): void {
  if (typeof adapter !== "object" || adapter === null || typeof adapter.start !== "function")
    throw new SandboxManagerError(
      "invalid_options",
      "SandboxManager requires a SandboxAdapter with a start() method.",
    );
}

/**
 * Stable JSON text for adapter options: object keys are sorted so equal
 * configurations with different insertion order compare equal.
 *
 * Throws for values JSON cannot represent (functions, symbols, bigints,
 * circular references, class instances) so callers never treat two
 * unrepresentable values as equal.
 */
function canonicalOptions(value: unknown, stack: Set<object> = new Set()): string {
  if (value === null) return "null";
  switch (typeof value) {
    case "string":
      return JSON.stringify(value);
    case "number":
      return Number.isFinite(value) ? String(value) : `number:${String(value)}`;
    case "boolean":
      return value ? "true" : "false";
    case "undefined":
      return "undefined";
    case "object": {
      if (stack.has(value)) throw new TypeError("adapterOptions contains a circular reference");
      stack.add(value);
      try {
        if (Array.isArray(value)) return `[${value.map((item) => canonicalOptions(item, stack)).join(",")}]`;
        const prototype = Object.getPrototypeOf(value);
        if (prototype !== Object.prototype && prototype !== null)
          throw new TypeError("adapterOptions contains a non-plain object");
        const record = value as Record<string, unknown>;
        const entries = Object.keys(record)
          .sort()
          .map((key) => `${JSON.stringify(key)}:${canonicalOptions(record[key], stack)}`);
        return `{${entries.join(",")}}`;
      } finally {
        stack.delete(value);
      }
    }
    default:
      throw new TypeError(`adapterOptions contains an unsupported ${typeof value} value`);
  }
}

function adapterOptionsEqual(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  try {
    return canonicalOptions(left) === canonicalOptions(right);
  } catch {
    return false;
  }
}

/**
 * Resource registry for in-process sandboxes, keyed by `sandboxId`.
 *
 * The manager knows nothing about chat sessions or a concrete sandbox
 * implementation: it neither receives a session key nor stores a session
 * mapping, and all lifecycle work is delegated to the injected
 * {@link SandboxAdapter}. Callers decide which session binds to which handle,
 * and {@link SandboxHandle.mcpServer} merely exposes a ready sandbox as data;
 * session init is assembled by the caller.
 */
export class SandboxManager<TOptions = unknown> {
  private readonly options: NormalizedOptions<TOptions>;
  private readonly handles = new Map<string, SandboxHandle<TOptions>>();
  private readonly startConfigs = new Map<string, SandboxHandleConfig<TOptions>>();

  constructor(options: SandboxManagerOptions<TOptions>) {
    assertAdapter(options?.adapter);
    const serverName = options.serverName ?? DEFAULT_SANDBOX_SERVER_NAME;
    assertServerName(serverName);
    this.options = {
      adapter: options.adapter,
      ...(options.adapterOptions === undefined ? {} : { adapterOptions: options.adapterOptions }),
      serverName,
      startupTimeoutMs: positiveTimeout(options.startupTimeoutMs, DEFAULT_STARTUP_TIMEOUT_MS),
      stopTimeoutMs: positiveTimeout(options.stopTimeoutMs, DEFAULT_STOP_TIMEOUT_MS),
      ...(options.signal === undefined ? {} : { signal: options.signal }),
    };
  }

  /** Number of handles currently owned (closed handles are removed). */
  get size(): number {
    return this.handles.size;
  }

  /** Look up a live handle; closed handles are not returned. */
  get(sandboxId: string): SandboxHandle<TOptions> | undefined {
    return this.handles.get(sandboxId);
  }

  /** Snapshot of the current handles in creation order. */
  list(): SandboxHandle<TOptions>[] {
    return [...this.handles.values()];
  }

  /**
   * Start a sandbox and return its handle.
   *
   * The same `sandboxId` is idempotent when explicit options match the values
   * the handle was created with: the existing handle is started and returned
   * again, inheriting omitted fields. An explicit mismatch is rejected with
   * code `conflict`. `signal` applies to this start only and is ignored by the
   * conflict check. A failed creation removes the half-built handle and
   * rethrows. Without `sandboxId` the manager generates a fresh id, so each
   * such call starts a distinct sandbox.
   */
  async start(options: SandboxManagerStartOptions<TOptions> = {}): Promise<SandboxHandle<TOptions>> {
    const sandboxId = options.sandboxId ?? createSandboxId();
    const existing = this.handles.get(sandboxId);
    if (existing) {
      const config = this.startConfigs.get(sandboxId);
      if (config) this.assertStartCompatible(config, options);
      await existing.start(options.signal);
      return existing;
    }

    const handle = this.createHandle(sandboxId, options);
    this.handles.set(sandboxId, handle);
    try {
      await handle.start(options.signal);
      return handle;
    } catch (error) {
      if (this.handles.get(sandboxId) === handle) {
        this.handles.delete(sandboxId);
        this.startConfigs.delete(sandboxId);
      }
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

  private createHandle(sandboxId: string, options: SandboxManagerStartOptions<TOptions>): SandboxHandle<TOptions> {
    const serverName = options.serverName ?? this.options.serverName;
    assertServerName(serverName);
    const adapterOptions = options.adapterOptions === undefined ? this.options.adapterOptions : options.adapterOptions;
    const config: SandboxHandleConfig<TOptions> = {
      sandboxId,
      serverName,
      ...(adapterOptions === undefined ? {} : { adapterOptions }),
      startupTimeoutMs: positiveTimeout(options.startupTimeoutMs, this.options.startupTimeoutMs),
      stopTimeoutMs: positiveTimeout(options.stopTimeoutMs, this.options.stopTimeoutMs),
      ...(this.options.signal === undefined ? {} : { signal: this.options.signal }),
    };
    const handle = new SandboxHandle<TOptions>(config, {
      getAdapter: () => this.options.adapter,
      onClosed: (closed) => {
        if (this.handles.get(closed.sandboxId) === closed) {
          this.handles.delete(closed.sandboxId);
          this.startConfigs.delete(closed.sandboxId);
        }
      },
    });
    this.startConfigs.set(sandboxId, config);
    return handle;
  }

  private assertStartCompatible(
    config: SandboxHandleConfig<TOptions>,
    options: SandboxManagerStartOptions<TOptions>,
  ): void {
    const mismatched: string[] = [];

    if (options.serverName !== undefined) {
      assertServerName(options.serverName);
      if (options.serverName !== config.serverName) mismatched.push("serverName");
    }
    if (options.adapterOptions !== undefined && !adapterOptionsEqual(options.adapterOptions, config.adapterOptions))
      mismatched.push("adapterOptions");
    if (
      options.startupTimeoutMs !== undefined &&
      positiveTimeout(options.startupTimeoutMs, config.startupTimeoutMs) !== config.startupTimeoutMs
    )
      mismatched.push("startupTimeoutMs");
    if (
      options.stopTimeoutMs !== undefined &&
      positiveTimeout(options.stopTimeoutMs, config.stopTimeoutMs) !== config.stopTimeoutMs
    )
      mismatched.push("stopTimeoutMs");

    if (mismatched.length === 0) return;
    throw new SandboxManagerError(
      "conflict",
      `Sandbox "${config.sandboxId}" is already managed with different ${mismatched.join(", ")}; ` +
        "close the existing handle before changing them.",
    );
  }
}
