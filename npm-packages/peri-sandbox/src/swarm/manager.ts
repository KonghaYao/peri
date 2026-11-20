import { lstatSync, mkdirSync, realpathSync, rmSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { isAbsolute, join, resolve } from 'node:path';
import { deriveSandboxId } from '../instance/options.js';
import { startSandbox } from '../instance/start.js';
import { SandboxError, type SandboxInstance, type StartSandboxOptions } from '../instance/types.js';
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

/** A logical environment whose sandbox identity is allocated but not started. */
export type SwarmReservationState = 'reserved' | 'starting' | 'failed';

export interface SwarmReservation {
  readonly logicalEnvId: string;
  readonly sandboxId: string;
  readonly workspacePath: string;
  readonly lastRunId: string | null;
  state: SwarmReservationState;
  lastUsedAt: Date;
  lastError: { code: string; message: string } | null;
}

/** Externally visible lifecycle of one logical environment. */
export interface SwarmSandboxDescription {
  readonly logicalEnvId: string;
  readonly sandboxId: string;
  readonly state: SwarmReservationState | 'ready' | 'stopped';
  readonly runId: string | null;
  readonly workspacePath: string;
  readonly lastUsedAt: string;
  readonly lastError: { code: string; message: string } | null;
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
 * `reserve()` allocates (or reuses) the sandbox identity without starting SRT;
 * `ensure()` singles-flights concurrent starts. Closing stops the instance and
 * keeps `<root>/<sandboxId>`; only `deleteData: true` removes persisted data.
 */
export class SwarmManager {
  readonly root: string;
  private readonly startSandbox: (options: StartSandboxOptions) => Promise<SandboxInstance>;
  private readonly bindings: SwarmBindingsStore;
  private readonly now: () => Date;
  private readonly entries = new Map<string, SwarmSandboxEntry>();
  private readonly reservations = new Map<string, SwarmReservation>();
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

  reservation(logicalEnvId: string): SwarmReservation | undefined {
    return this.reservations.get(logicalEnvId);
  }

  binding(logicalEnvId: string): SwarmBinding | undefined {
    return this.bindings.get(logicalEnvId);
  }

  /** Whether a real MCP request may start this environment. */
  canStart(logicalEnvId: string): boolean {
    if (this.reservations.has(logicalEnvId) || this.starting.has(logicalEnvId)) return true;
    // A binding without a run id is a reservation that survived a restart; a
    // binding with a run id was started before and stays stopped (S7).
    return this.bindings.get(logicalEnvId)?.lastRunId === null;
  }

  /** External state for `GET`/`POST`; `undefined` means "no such environment". */
  describe(logicalEnvId: string): SwarmSandboxDescription | undefined {
    const entry = this.entries.get(logicalEnvId);
    if (entry !== undefined) {
      return {
        logicalEnvId,
        sandboxId: entry.sandboxId,
        state: 'ready',
        runId: entry.runId,
        workspacePath: entry.workspacePath,
        lastUsedAt: entry.lastUsedAt.toISOString(),
        lastError: null,
      };
    }
    const reservation = this.reservations.get(logicalEnvId);
    if (reservation !== undefined) {
      return {
        logicalEnvId,
        sandboxId: reservation.sandboxId,
        state: reservation.state,
        runId: reservation.lastRunId,
        workspacePath: reservation.workspacePath,
        lastUsedAt: reservation.lastUsedAt.toISOString(),
        lastError: reservation.lastError,
      };
    }
    const binding = this.bindings.get(logicalEnvId);
    if (binding === undefined) return undefined;
    return {
      logicalEnvId,
      sandboxId: binding.sandboxId,
      state: binding.lastRunId === null ? 'reserved' : 'stopped',
      runId: binding.lastRunId,
      workspacePath: binding.workspacePath,
      lastUsedAt: binding.lastUsedAt,
      lastError: null,
    };
  }

  /**
   * Allocate (or reuse) the sandbox identity for a logical environment.
   *
   * No process is started and no sandbox directory is created: the first real
   * MCP request calls [`ensure`] and starts SRT then.
   */
  async reserve(logicalEnvId: string): Promise<SwarmSandboxDescription> {
    assertLogicalEnvId(logicalEnvId);
    const described = this.describe(logicalEnvId);
    if (
      described !== undefined &&
      (described.state === 'reserved' || described.state === 'starting' || described.state === 'ready')
    )
      return described;
    if (this.starting.has(logicalEnvId)) {
      const pending = this.starting.get(logicalEnvId);
      if (pending !== undefined) {
        await pending.catch(() => undefined);
        const settled = this.describe(logicalEnvId);
        if (settled !== undefined) return settled;
      }
    }
    const reservation = this.reservations.get(logicalEnvId) ?? this.newReservation(logicalEnvId);
    reservation.state = 'reserved';
    reservation.lastError = null;
    reservation.lastUsedAt = this.now();
    this.reservations.set(logicalEnvId, reservation);
    await this.bindings.record(logicalEnvId, {
      sandboxId: reservation.sandboxId,
      workspacePath: reservation.workspacePath,
      lastRunId: reservation.lastRunId,
      lastUsedAt: reservation.lastUsedAt.toISOString(),
    });
    const view = this.describe(logicalEnvId);
    if (view === undefined) throw new SwarmError('invalid_config', 'sandbox reservation was not recorded.');
    return view;
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
    const reservation = this.reservations.get(logicalEnvId);
    if (entry === undefined && binding === undefined && reservation === undefined)
      return { found: false, closed: false, deletedData: false };
    const deleteData = options.deleteData === true;
    if (entry !== undefined) {
      await entry.instance.close({ deleteData });
      this.entries.delete(logicalEnvId);
    } else if (deleteData) {
      // A reservation never started SRT, but a failed start or a previous run
      // may have left `<root>/<sandboxId>` behind.
      const sandboxId = reservation?.sandboxId ?? binding?.sandboxId;
      if (sandboxId !== undefined) rmSync(join(this.root, sandboxId), { recursive: true, force: true });
    }
    this.reservations.delete(logicalEnvId);
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
    this.reservations.clear();
    if (failures.length > 0) throw new AggregateError(failures, 'some swarm sandboxes did not close');
  }

  private newReservation(logicalEnvId: string): SwarmReservation {
    const stored = this.bindings.get(logicalEnvId);
    const sandboxId = stored?.sandboxId ?? deriveSandboxId(logicalEnvId);
    return {
      logicalEnvId,
      sandboxId,
      workspacePath: stored?.workspacePath ?? join(this.root, sandboxId, 'workspace'),
      lastRunId: null,
      state: 'reserved',
      lastUsedAt: this.now(),
      lastError: null,
    };
  }

  private async startEntry(logicalEnvId: string): Promise<SwarmSandboxEntry> {
    const previous = this.entries.get(logicalEnvId);
    if (previous !== undefined) {
      await previous.instance.close({ deleteData: false });
      this.entries.delete(logicalEnvId);
    }
    const reservation = this.reservations.get(logicalEnvId) ?? this.newReservation(logicalEnvId);
    this.reservations.set(logicalEnvId, reservation);
    reservation.state = 'starting';
    reservation.lastError = null;
    const sandboxId = reservation.sandboxId;
    let instance: SandboxInstance;
    try {
      instance = await this.startSandbox({ sandboxId, sandboxesRoot: this.root });
    } catch (error) {
      reservation.state = 'failed';
      reservation.lastError = {
        code: error instanceof SandboxError ? error.code : 'start_failed',
        message: error instanceof Error ? error.message : 'sandbox start failed',
      };
      throw error;
    }
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
      reservation.state = 'failed';
      reservation.lastError = {
        code: error instanceof SwarmError ? error.code : 'bindings_invalid',
        message: error instanceof Error ? error.message : 'cannot record the sandbox binding',
      };
      this.reservations.set(logicalEnvId, reservation);
      await instance.close({ deleteData: false }).catch(() => undefined);
      throw error;
    }
    this.reservations.delete(logicalEnvId);
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
