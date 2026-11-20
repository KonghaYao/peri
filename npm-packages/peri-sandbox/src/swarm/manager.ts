import { lstatSync, mkdirSync, realpathSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { isAbsolute, join, resolve } from 'node:path';
import { deriveSandboxId } from '../instance/options.js';
import { startSandbox } from '../instance/start.js';
import type { SandboxInstance, StartSandboxOptions } from '../instance/types.js';
import { type SwarmBinding, SwarmBindingsStore } from './bindings.js';
import { assertLogicalEnvId, SwarmError } from './errors.js';

/** Default persistence root for local swarm sandboxes. */
export function defaultSwarmSandboxesRoot(): string {
  return join(homedir(), '.peri-swarm', 'sandboxes');
}

/** Resolve `PERI_SWARM_SANDBOXES_ROOT`; empty values fall back to the default root. */
export function resolveSwarmSandboxesRoot(value?: string): string {
  if (value === undefined || value.length === 0) return defaultSwarmSandboxesRoot();
  if (!isAbsolute(value)) throw new SwarmError('invalid_config', 'PERI_SWARM_SANDBOXES_ROOT must be an absolute path.');
  return resolve(value);
}

/** Create and verify the private root; returns the canonical path used by every sandbox. */
export function prepareSwarmRoot(root: string): string {
  mkdirSync(root, { recursive: true, mode: 0o700 });
  const entry = lstatSync(root);
  if (entry.isSymbolicLink() || !statSync(root).isDirectory())
    throw new SwarmError('invalid_config', `sandboxes root must be a real directory: ${root}`);
  if (statSync(root).mode & 0o077)
    throw new SwarmError('invalid_config', `sandboxes root must be private (0700): ${root}`);
  return realpathSync(root);
}

/** One logical environment mapped to a running sandbox instance. */
export interface SwarmSandboxEntry {
  readonly logicalEnvId: string;
  readonly sandboxId: string;
  readonly runId: string;
  readonly workspacePath: string;
  readonly mcpUrl: string;
  /** Inner MCP bearer token; memory only, never persisted or exposed over HTTP. */
  readonly innerToken: string;
  readonly instance: SandboxInstance;
  readonly lastUsedAt: Date;
}

export interface SwarmManagerOptions {
  /** Deployment root; defaults to `defaultSwarmSandboxesRoot()`. */
  readonly root?: string;
  /** Test seam; defaults to `startSandbox()`. */
  readonly startSandbox?: (options: StartSandboxOptions) => Promise<SandboxInstance>;
  readonly now?: () => Date;
}

export interface CloseSwarmSandboxResult {
  readonly found: boolean;
  readonly closed: boolean;
  readonly deletedData: boolean;
}

/**
 * Process-local registry: logical environment id → one started sandbox.
 *
 * `ensure()` singles-flights concurrent starts. Closing stops the instance and
 * keeps `<root>/<sandboxId>`; only `deleteData: true` removes persisted data.
 */
export class SwarmManager {
  readonly root: string;
  private readonly startSandbox: (options: StartSandboxOptions) => Promise<SandboxInstance>;
  private readonly bindings: SwarmBindingsStore;
  private readonly now: () => Date;
  private readonly entries = new Map<string, SwarmSandboxEntry>();
  private readonly starting = new Map<string, Promise<SwarmSandboxEntry>>();

  constructor(options: SwarmManagerOptions = {}) {
    this.root = prepareSwarmRoot(resolveSwarmSandboxesRoot(options.root));
    this.startSandbox = options.startSandbox ?? startSandbox;
    this.now = options.now ?? (() => new Date());
    this.bindings = new SwarmBindingsStore(this.root);
  }

  get(logicalEnvId: string): SwarmSandboxEntry | undefined {
    return this.entries.get(logicalEnvId);
  }

  binding(logicalEnvId: string): SwarmBinding | undefined {
    return this.bindings.get(logicalEnvId);
  }

  async ensure(logicalEnvId: string): Promise<SwarmSandboxEntry> {
    assertLogicalEnvId(logicalEnvId);
    const existing = this.entries.get(logicalEnvId);
    if (existing !== undefined && existing.instance.state === 'ready') {
      await this.persist(existing, this.now());
      return existing;
    }
    const pending = this.starting.get(logicalEnvId);
    if (pending !== undefined) return await pending;
    const task = this.startEntry(logicalEnvId);
    this.starting.set(logicalEnvId, task);
    try {
      return await task;
    } finally {
      if (this.starting.get(logicalEnvId) === task) this.starting.delete(logicalEnvId);
    }
  }

  async close(logicalEnvId: string, options: { deleteData?: boolean } = {}): Promise<CloseSwarmSandboxResult> {
    assertLogicalEnvId(logicalEnvId);
    const pending = this.starting.get(logicalEnvId);
    if (pending !== undefined) await pending.catch(() => undefined);
    const entry = this.entries.get(logicalEnvId);
    const binding = this.bindings.get(logicalEnvId);
    if (entry === undefined && binding === undefined) return { found: false, closed: false, deletedData: false };
    const deleteData = options.deleteData === true;
    if (entry !== undefined) {
      await entry.instance.close({ deleteData });
      this.entries.delete(logicalEnvId);
    }
    if (deleteData) {
      await this.bindings.forget(logicalEnvId);
    } else if (binding !== undefined) {
      await this.bindings.record(logicalEnvId, { ...binding, lastUsedAt: this.now().toISOString() });
    }
    return { found: true, closed: entry !== undefined, deletedData: deleteData };
  }

  /** Stop every running instance without deleting persisted workspaces. */
  async closeAll(): Promise<void> {
    await Promise.allSettled([...this.starting.values()]);
    const failures: unknown[] = [];
    for (const [logicalEnvId, entry] of [...this.entries]) {
      try {
        await entry.instance.close({ deleteData: false });
        this.entries.delete(logicalEnvId);
      } catch (error) {
        failures.push(error);
      }
    }
    if (failures.length > 0) throw new AggregateError(failures, 'some swarm sandboxes did not close');
  }

  private async startEntry(logicalEnvId: string): Promise<SwarmSandboxEntry> {
    const previous = this.entries.get(logicalEnvId);
    if (previous !== undefined) {
      await previous.instance.close({ deleteData: false });
      this.entries.delete(logicalEnvId);
    }
    const stored = this.bindings.get(logicalEnvId);
    const sandboxId = stored?.sandboxId ?? deriveSandboxId(logicalEnvId);
    const instance = await this.startSandbox({ sandboxId, sandboxesRoot: this.root });
    const entry: SwarmSandboxEntry = {
      logicalEnvId,
      sandboxId,
      runId: instance.runId,
      workspacePath: instance.workspacePath,
      mcpUrl: instance.mcpUrl,
      innerToken: instance.token,
      instance,
      lastUsedAt: this.now(),
    };
    this.entries.set(logicalEnvId, entry);
    try {
      await this.persist(entry, entry.lastUsedAt);
    } catch (error) {
      this.entries.delete(logicalEnvId);
      await instance.close({ deleteData: false }).catch(() => undefined);
      throw error;
    }
    return entry;
  }

  private persist(entry: SwarmSandboxEntry, lastUsedAt: Date): Promise<void> {
    return this.bindings.record(entry.logicalEnvId, {
      sandboxId: entry.sandboxId,
      workspacePath: entry.workspacePath,
      lastRunId: entry.runId,
      lastUsedAt: lastUsedAt.toISOString(),
    });
  }
}
