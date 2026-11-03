import { deleteSandboxData, type SandboxLock } from './workspace.js';
import { withTimeout } from '../util/timeout.js';
import {
  SandboxError,
  type McpBackendPort,
  type SandboxErrorCode,
  type SandboxInstance,
  type SandboxRuntimeInfo,
  type SandboxState,
  type SandboxStatus,
} from './types.js';
import type { SandboxHttpServer } from '../server/http.js';

export interface ManagedSandboxOptions {
  sandboxId: string;
  runId: string;
  token: string;
  tokenFile: string | null;
  sandboxRoot: string;
  sandboxesRoot: string;
  workspacePath: string;
  runtime: SandboxRuntimeInfo;
  http: SandboxHttpServer;
  backend: McpBackendPort;
  lock: SandboxLock;
  stopTimeoutMs: number;
  stop: () => Promise<void>;
}

function asSandboxError(error: unknown, fallback: SandboxErrorCode): SandboxError {
  if (error instanceof SandboxError) return error;
  return new SandboxError(fallback, error instanceof Error ? error.message : String(error), { cause: error });
}

/**
 * One started sandbox instance.
 *
 * `close()` is idempotent and keeps `<sandboxesRoot>/<sandboxId>` unless
 * `deleteData` is requested. The lock is released only after the provider
 * confirmed the unit/process group stopped.
 */
export class ManagedSandboxInstance implements SandboxInstance {
  readonly id: string;
  readonly runId: string;
  readonly token: string;
  readonly tokenFile: string | null;
  readonly workspacePath: string;
  readonly sandboxRoot: string;
  readonly runtime: SandboxRuntimeInfo;
  private readonly sandboxesRoot: string;
  private readonly http: SandboxHttpServer;
  private readonly backend: McpBackendPort;
  private readonly lock: SandboxLock;
  private readonly stopTimeoutMs: number;
  private readonly stopTransport: () => Promise<void>;
  private stateValue: SandboxState = 'ready';
  private lastErrorValue: { code: SandboxErrorCode; message: string } | null = null;
  private closePromise: Promise<void> | null = null;
  private dataDeleted = false;

  constructor(options: ManagedSandboxOptions) {
    this.id = options.sandboxId;
    this.runId = options.runId;
    this.token = options.token;
    this.tokenFile = options.tokenFile;
    this.workspacePath = options.workspacePath;
    this.sandboxRoot = options.sandboxRoot;
    this.runtime = Object.freeze({ ...options.runtime });
    this.sandboxesRoot = options.sandboxesRoot;
    this.http = options.http;
    this.backend = options.backend;
    this.lock = options.lock;
    this.stopTimeoutMs = options.stopTimeoutMs;
    this.stopTransport = options.stop;
  }

  get state(): SandboxState {
    return this.stateValue;
  }

  get mcpUrl(): string {
    return this.http.url;
  }

  status(): SandboxStatus {
    return {
      id: this.id,
      runId: this.runId,
      state: this.stateValue,
      mcpUrl: this.http.url,
      workspacePath: this.workspacePath,
      sandboxRoot: this.sandboxRoot,
      runtime: { ...this.runtime },
      lastError: this.lastErrorValue ? { ...this.lastErrorValue } : null,
    };
  }

  /** Mark the backend as unexpectedly gone; HTTP starts returning 503. */
  markBackendExited(error: Error): void {
    if (this.stateValue === 'closed' || this.stateValue === 'stopping') return;
    this.stateValue = 'failed';
    this.lastErrorValue = { code: 'backend_exited', message: error.message };
  }

  close(options: { deleteData?: boolean } = {}): Promise<void> {
    const deleteData = options.deleteData === true;
    if (this.stateValue === 'closed') {
      if (deleteData && !this.dataDeleted) return this.deleteData();
      return Promise.resolve();
    }
    if (this.closePromise) {
      return this.closePromise.then(() => {
        if (deleteData && !this.dataDeleted) return this.deleteData();
      });
    }
    const operation = this.performClose(deleteData).finally(() => {
      if (this.closePromise === operation) this.closePromise = null;
    });
    this.closePromise = operation;
    return operation;
  }

  private async performClose(deleteData: boolean): Promise<void> {
    this.stateValue = 'stopping';
    const failures: unknown[] = [];
    try {
      await withTimeout(
        this.http.close(),
        this.stopTimeoutMs,
        () => new SandboxError('stop_incomplete', 'Sandbox HTTP close timed out.'),
      );
    } catch (error) {
      failures.push(asSandboxError(error, 'stop_incomplete'));
    }
    try {
      await withTimeout(
        this.backend.close(),
        this.stopTimeoutMs,
        () => new SandboxError('stop_incomplete', 'Sandbox backend close timed out.'),
      );
    } catch {
      // The provider stop below is the authority for process cleanup.
    }
    try {
      await withTimeout(
        this.stopTransport(),
        this.stopTimeoutMs,
        () => new SandboxError('stop_incomplete', 'Sandbox stop timed out.'),
      );
    } catch (error) {
      failures.push(asSandboxError(error, 'stop_incomplete'));
    }
    if (failures.length > 0) {
      this.stateValue = 'failed';
      this.lastErrorValue = { code: 'stop_incomplete', message: 'Sandbox close was not confirmed.' };
      throw new SandboxError('stop_incomplete', 'Sandbox close was not confirmed.', {
        cause: new AggregateError(failures),
      });
    }
    this.lock.release();
    this.stateValue = 'closed';
    this.lastErrorValue = null;
    if (deleteData) await this.deleteData();
  }

  private async deleteData(): Promise<void> {
    try {
      await deleteSandboxData({ sandboxId: this.id, sandboxesRoot: this.sandboxesRoot });
      this.dataDeleted = true;
    } catch (error) {
      const mapped = asSandboxError(error, 'delete_incomplete');
      this.lastErrorValue = { code: mapped.code, message: mapped.message };
      throw mapped;
    }
  }
}
