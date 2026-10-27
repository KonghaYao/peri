import { randomBytes } from 'node:crypto';
import { mkdirSync, writeFileSync, rmSync, statfsSync } from 'node:fs';
import { join } from 'node:path';
import type { Config } from '../config.js';
import { Registry, publicRecord } from './registry.js';
import { makeDirectories, policyFor, sandboxPath, checkDirectory, removeMacTmp } from '../runtime/policy.js';
import type { Provider, Handle } from '../runtime/provider.js';
import type { SandboxRecord, StoredSandbox, SandboxControl } from './types.js';
import { ControlError } from './types.js';

export interface Backend {
  close(): Promise<void>;
}
export type OpenBackend = (id: string, generation: string, handle: Handle) => Promise<Backend>;

export class Control implements SandboxControl {
  private readonly pending = new Map<string, Promise<unknown>>();
  private readonly startups = new Map<string, { promise: Promise<void>; cancelled: boolean }>();
  private readonly stoppings = new Map<string, Promise<StoredSandbox>>();
  private readonly destroying = new Set<string>();
  readonly backends = new Map<string, Backend>();
  private readonly serial = new Map<string, Promise<void>>();
  constructor(
    readonly config: Config,
    readonly registry: Registry,
    readonly provider: Provider,
    private readonly openBackend: OpenBackend,
  ) {}

  private async lock<T>(id: string, action: () => Promise<T> | T): Promise<T> {
    const prior = this.serial.get(id) ?? Promise.resolve();
    let release!: () => void;
    const next = new Promise<void>((resolve) => {
      release = resolve;
    });
    this.serial.set(id, next);
    await prior;
    try {
      return await action();
    } finally {
      release();
      if (this.serial.get(id) === next) this.serial.delete(id);
    }
  }

  private async operation(
    name: string,
    input: unknown,
    requestId: string,
    id: string | undefined,
    action: () => Promise<StoredSandbox>,
  ): Promise<SandboxRecord> {
    if (this.registry.unhealthy && name !== 'stop') throw new ControlError('interrupted', 'registry unavailable');
    let prior;
    try {
      prior = this.registry.begin(requestId, name, input, id);
    } catch (error) {
      if (name !== 'stop') throw error;
      await action();
      throw new ControlError('interrupted', `cleanup attempted without database receipt: ${String(error)}`);
    }
    if (prior) {
      if (prior.status === 'pending' && this.pending.has(requestId))
        return this.pending.get(requestId) as Promise<SandboxRecord>;
      if (prior.status === 'pending')
        throw new ControlError('interrupted', 'operation unresolved; inspect current state');
      if (prior.status !== 'done') {
        const err = JSON.parse(prior.result ?? '{}');
        throw new ControlError(err.code ?? 'interrupted', err.message ?? 'operation failed');
      }
      return JSON.parse(prior.result ?? '{}') as SandboxRecord;
    }
    const promise = (async () => {
      try {
        const record = await action();
        const result = publicRecord(record);
        this.registry.finish(requestId, 'done', result, record.id);
        try {
          this.provider.pruneStopped?.(
            new Set(this.registry.list(true).flatMap((item) => (item.unit ? [item.unit] : []))),
          );
        } catch (error) {
          console.error('process record pruning failed', error);
        }
        return result;
      } catch (error) {
        const failure = error instanceof ControlError ? error : new ControlError('interrupted', String(error));
        try {
          this.registry.finish(requestId, 'failed', { code: failure.code, message: failure.message }, id);
        } catch (writeFailure) {
          console.error('operation receipt persistence failed', writeFailure);
        }
        throw failure;
      } finally {
        this.pending.delete(requestId);
      }
    })();
    this.pending.set(requestId, promise);
    return promise;
  }

  async create(spec: { name?: string; profile: string }, options: { requestId: string }): Promise<SandboxRecord> {
    return this.operation('create', spec, options.requestId, undefined, () =>
      this.lock('__create__', () => {
        if (this.registry.list().length >= this.config.limits.maxSandboxes)
          throw new ControlError('limit_exceeded', 'sandbox limit reached');
        let id = '';
        for (let attempt = 0; attempt < 16; attempt++) {
          const candidate = `sbx_${randomBytes(16).toString('hex')}`;
          if (!this.registry.get(candidate)) {
            id = candidate;
            break;
          }
        }
        if (!id) throw new ControlError('conflict', 'could not allocate unique sandbox ID');
        const { json, hash } = policyFor(this.config, id, spec.profile);
        const createdAt = new Date().toISOString();
        const record: StoredSandbox = {
          id,
          name: spec.name,
          profile: spec.profile,
          state: 'stopped',
          desiredState: 'stopped',
          mcpUrl: `${this.config.publicBaseUrl.replace(/\/$/, '')}/sandboxes/${id}/mcp`,
          policyJson: json,
          policyHash: hash,
          resourceJson: JSON.stringify(this.config.profiles[spec.profile]),
          createdAt,
          updatedAt: createdAt,
        };
        this.registry.put(record);
        makeDirectories(this.config, id);
        return record;
      }),
    );
  }

  async start(id: string, options: { requestId: string }): Promise<SandboxRecord> {
    return this.operation('start', { id }, options.requestId, id, async () => {
      let resolveStartup!: () => void;
      const startup = {
        promise: new Promise<void>((resolve) => {
          resolveStartup = resolve;
        }),
        cancelled: false,
      };
      const prepared = await this.lock(id, () => {
        const record = this.require(id);
        if (this.stoppings.has(id)) throw new ControlError('startup_cancelled', 'start cancelled by stop');
        if (this.destroying.has(id)) throw new ControlError('deleted', 'destroy in progress');
        if (record.state === 'starting' && this.startups.has(id))
          return { record, existing: false, joining: this.startups.get(id)!.promise };
        if (record.state === 'ready') return { record, existing: true };
        if (record.state === 'deleted' || record.state === 'destroying') throw new ControlError('deleted', id);
        if (record.state !== 'stopped' && record.state !== 'failed')
          throw new ControlError('conflict', `cannot start from ${record.state}`);
        const filesystem = statfsSync(this.config.sandboxesRoot, { bigint: true });
        if (filesystem.bavail * filesystem.bsize < BigInt(this.config.limits.minFreeBytes ?? 1073741824)) {
          throw new ControlError('limit_exceeded', 'sandbox data filesystem is below the free-space watermark');
        }
        record.generation = `run_${randomBytes(12).toString('hex')}`;
        record.unit = this.provider.unit(id, record.generation);
        record.state = 'starting';
        record.desiredState = 'running';
        record.lastError = undefined;
        this.registry.put(record);
        this.startups.set(id, startup);
        return { record, existing: false };
      });
      if (prepared.existing) return prepared.record;
      if (prepared.joining) {
        await prepared.joining;
        const record = this.require(id);
        if (record.state !== 'ready') throw new ControlError('startup_failed', 'shared startup did not become ready');
        return record;
      }
      const generation = prepared.record.generation!;
      const worker = (async () => {
        let backend: Backend | undefined;
        let abandoned = false;
        try {
          const policyPath = join(this.config.controlRoot, 'policies', `${id}.json`);
          mkdirSync(join(this.config.controlRoot, 'policies'), { recursive: true, mode: 0o700 });
          writeFileSync(policyPath, prepared.record.policyJson, { mode: 0o600 });
          if (startup.cancelled) throw new ControlError('startup_cancelled', 'start cancelled before launch');
          if (this.registry.unhealthy) throw new ControlError('interrupted', 'registry unavailable before launch');
          let launchExpired = false;
          const launching = this.provider.start(id, generation, policyPath, JSON.parse(prepared.record.resourceJson));
          void launching.then(
            (late) => {
              if (launchExpired)
                void this.provider
                  .stop(late.unit)
                  .catch((error) => console.error('late launch unit cleanup failed:', late.unit, error));
            },
            () => {},
          );
          let launchTimer!: NodeJS.Timeout;
          const handle = await Promise.race([
            launching,
            new Promise<never>((_, reject) => {
              launchTimer = setTimeout(() => {
                launchExpired = true;
                reject(new ControlError('startup_failed', 'provider launch timeout'));
              }, this.config.limits.startupMs);
            }),
          ]).finally(() => clearTimeout(launchTimer));
          if (this.registry.unhealthy) throw new ControlError('interrupted', 'registry unavailable after launch');
          let exited = handle.process.exitCode !== null && handle.process.exitCode !== undefined;
          handle.process.once('close', () => {
            exited = true;
            void this.handleExit(id, generation);
          });
          const opening = this.openBackend(id, generation, handle);
          void opening.then(
            (backend) => {
              if (abandoned) void backend.close().catch((error) => console.error('late backend close failed', error));
            },
            () => {},
          );
          let timer!: NodeJS.Timeout;
          backend = await Promise.race([
            opening,
            new Promise<never>((_, reject) => {
              timer = setTimeout(() => {
                abandoned = true;
                reject(new ControlError('startup_failed', 'backend readiness timeout'));
              }, this.config.limits.startupMs);
            }),
          ]).finally(() => clearTimeout(timer));
          if (exited) throw new ControlError('startup_failed', 'unit exited before readiness');
          if (startup.cancelled) throw new ControlError('startup_cancelled', 'start cancelled by stop');
          if (this.registry.unhealthy) throw new ControlError('interrupted', 'registry unavailable during startup');
          const readyBackend = backend;
          await this.lock(id, () => {
            const record = this.require(id);
            if (exited) throw new ControlError('startup_failed', 'unit exited before readiness');
            if (record.generation !== generation || record.state !== 'starting')
              throw new ControlError('startup_cancelled', 'generation superseded');
            record.state = 'ready';
            record.appliedHash = record.policyHash;
            this.registry.put(record);
            this.backends.set(id, readyBackend);
          });
        } catch (error) {
          if (!backend) abandoned = true;
          if (backend) {
            try {
              await backend.close();
            } catch (failure) {
              console.error('backend close during startup failed', failure);
            }
          }
          let cleanupFailure: unknown;
          try {
            await this.provider.stop(prepared.record.unit!);
          } catch (failure) {
            cleanupFailure = failure;
          }
          try {
            await this.lock(id, () => {
              const record = this.require(id);
              if (record.generation === generation) {
                record.state = cleanupFailure ? 'stop_incomplete' : 'failed';
                record.desiredState = 'stopped';
                if (!cleanupFailure) {
                  record.unit = undefined;
                  record.generation = undefined;
                }
                record.lastError = { code: 'startup_failed', message: String(error) };
                this.registry.put(record);
              }
            });
          } catch (writeFailure) {
            console.error('startup cleanup persistence failed', writeFailure);
          }
          if (cleanupFailure) throw new ControlError('stop_incomplete', String(cleanupFailure));
          throw error;
        }
      })();
      try {
        await worker;
      } finally {
        resolveStartup();
        if (this.startups.get(id) === startup) this.startups.delete(id);
      }
      if (startup.cancelled) throw new ControlError('startup_cancelled', 'start cancelled by stop');
      return this.require(id);
    });
  }

  async stop(id: string, options: { requestId: string }): Promise<SandboxRecord> {
    return this.operation('stop', { id }, options.requestId, id, async () => {
      const existing = this.stoppings.get(id);
      if (existing) return existing;
      const stopping = (async () => {
        const active = await this.lock(id, () => this.startups.get(id));
        if (active) {
          active.cancelled = true;
          await active.promise.catch(() => {});
        }
        const { record, backend, persistenceFailure } = await this.lock(id, () => {
          const record = this.require(id);
          if (record.state === 'deleted') throw new ControlError('deleted', id);
          if (record.state === 'stopped' && !record.unit)
            return { record, backend: undefined, persistenceFailure: undefined };
          record.state = 'stopping';
          record.desiredState = 'stopped';
          let persistenceFailure: unknown;
          try {
            this.registry.put(record);
          } catch (error) {
            persistenceFailure = error;
          }
          const backend = this.backends.get(id);
          this.backends.delete(id);
          return { record, backend, persistenceFailure };
        });
        try {
          await backend?.close();
        } catch (error) {
          console.error('backend close failed; continuing unit stop', error);
        }
        if (record.unit) {
          try {
            await this.provider.stop(record.unit);
          } catch (error) {
            try {
              await this.lock(id, () => {
                const current = this.require(id);
                if (current.generation === record.generation) {
                  current.state = 'stop_incomplete';
                  this.registry.put(current);
                }
              });
            } catch (writeFailure) {
              console.error('cleanup status persistence failed', writeFailure);
            }
            throw new ControlError('stop_incomplete', String(error));
          }
        }
        if (persistenceFailure)
          throw new ControlError(
            'interrupted',
            `unit stopped, but database unavailable: ${String(persistenceFailure)}`,
          );
        return this.lock(id, () => {
          const current = this.require(id);
          if (current.generation !== record.generation || current.state === 'deleted') return current;
          current.state = 'stopped';
          current.unit = undefined;
          current.generation = undefined;
          this.registry.put(current);
          return current;
        });
      })();
      this.stoppings.set(id, stopping);
      try {
        return await stopping;
      } finally {
        if (this.stoppings.get(id) === stopping) this.stoppings.delete(id);
      }
    });
  }

  async destroy(id: string, options: { requestId: string; deleteData: true }): Promise<SandboxRecord> {
    if (options.deleteData !== true) throw new ControlError('conflict', 'deleteData: true required');
    return this.operation('destroy', { id, deleteData: true }, options.requestId, id, async () => {
      await this.lock(id, () => {
        if (this.destroying.has(id)) throw new ControlError('conflict', 'destroy in progress');
        this.require(id);
        this.destroying.add(id);
      });
      try {
        const current = this.require(id);
        if (current.state === 'deleted') return current;
        if (
          current.unit ||
          (current.state !== 'stopped' && current.state !== 'failed' && current.state !== 'destroying')
        ) {
          await this.stop(id, { requestId: `${options.requestId}:stop` });
        }
        return this.lock(id, () => {
          const record = this.require(id);
          if (record.unit) throw new ControlError('stop_incomplete', 'unit still registered');
          record.state = 'destroying';
          record.desiredState = 'deleted';
          this.registry.put(record);
          const base = sandboxPath(this.config, id);
          checkDirectory(base, this.config.sandboxesRoot);
          if (process.platform === 'darwin') removeMacTmp(id);
          rmSync(base, { force: true, recursive: true });
          record.state = 'deleted';
          this.registry.put(record);
          return record;
        });
      } finally {
        await this.lock(id, () => {
          this.destroying.delete(id);
        });
      }
    });
  }

  private require(id: string): StoredSandbox {
    const record = this.registry.get(id);
    if (!record) throw new ControlError('not_found', id);
    return record;
  }
  private async handleExit(id: string, generation: string): Promise<void> {
    const backend = await this.lock(id, () => {
      const record = this.require(id);
      if (record.generation !== generation || record.state !== 'ready') return undefined;
      const active = this.backends.get(id);
      this.backends.delete(id);
      return active;
    }).catch(() => undefined);
    if (!backend) return;
    try {
      await backend.close();
    } catch (error) {
      console.error('backend close after exit failed', error);
    }
    let failure: unknown;
    try {
      await this.provider.stop(this.provider.unit(id, generation));
    } catch (error) {
      failure = error;
    }
    try {
      await this.lock(id, () => {
        const record = this.require(id);
        if (record.generation !== generation || record.state === 'deleted') return;
        record.state = failure ? 'stop_incomplete' : 'failed';
        record.desiredState = 'stopped';
        if (!failure) {
          record.unit = undefined;
          record.generation = undefined;
        }
        record.lastError = {
          code: failure ? 'stop_incomplete' : 'interrupted',
          message: String(failure ?? 'backend exited'),
        };
        this.registry.put(record);
      });
    } catch (error) {
      console.error('backend exit cleanup persistence failed', error);
    }
  }
  async inspect(id: string): Promise<SandboxRecord> {
    return publicRecord(this.require(id));
  }
  async list(): Promise<SandboxRecord[]> {
    return this.registry.list().map(publicRecord);
  }
  async recover(): Promise<void> {
    let records: StoredSandbox[] = [];
    let readFailure: unknown;
    let owned: string[] = [];
    let discoveryFailure: unknown;
    try {
      records = this.registry.list(true);
    } catch (error) {
      readFailure = error;
    }
    try {
      owned = await this.provider.listOwned();
    } catch (error) {
      discoveryFailure = error;
    }
    const failures = new Map<string, unknown>();
    const units = new Set([...owned, ...records.flatMap((record) => (record.unit ? [record.unit] : []))]);
    for (const unit of units) {
      try {
        await this.provider.stop(unit);
      } catch (error) {
        failures.set(unit, error);
      }
    }
    if (readFailure || discoveryFailure)
      throw new ControlError(
        'stop_incomplete',
        `recovery cannot reconcile units: ${String(readFailure ?? discoveryFailure)}`,
      );
    for (const record of records) {
      if (record.state === 'deleted') continue;
      if (record.unit && failures.has(record.unit)) {
        record.state = 'stop_incomplete';
        record.lastError = { code: 'stop_incomplete', message: String(failures.get(record.unit)) };
      } else if (record.state !== 'destroying') {
        record.unit = undefined;
        record.generation = undefined;
        record.state = 'stopped';
        record.desiredState = 'stopped';
        makeDirectories(this.config, record.id);
      }
      this.registry.put(record);
    }
    this.registry.recoverReceipts();
    this.provider.pruneStopped?.(new Set(records.flatMap((item) => (item.unit ? [item.unit] : []))));
    if (failures.size) throw new ControlError('stop_incomplete', `recovery could not clean ${failures.size} unit(s)`);
  }
}
