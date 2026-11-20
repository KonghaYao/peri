import type { Database } from 'bun:sqlite';
import type { PlatformRepositories } from '../repositories';
import { AutomationScheduler } from './scheduler';
import { AutomationService } from './service';
import { type AutomationSessionPort, createSessionAutomationPort } from './session-port';

export interface AutomationRuntimeOptions {
  readonly repos: PlatformRepositories;
  readonly port: AutomationSessionPort | null;
  readonly now?: () => number;
  readonly tickIntervalMs?: number;
  readonly missedGraceMs?: number;
  readonly tick?: () => Promise<void>;
}

export interface AutomationRuntime {
  readonly repos: PlatformRepositories;
  readonly service: AutomationService;
  readonly scheduler: AutomationScheduler;
}

export interface InstalledAutomationRuntime {
  readonly runtime: AutomationRuntime;
  readonly stop: () => Promise<void>;
  readonly restore: () => void;
}

export function createAutomationRuntime(options: AutomationRuntimeOptions): AutomationRuntime {
  const service = new AutomationService({
    repos: options.repos,
    port: options.port,
    ...(options.now === undefined ? {} : { now: options.now }),
    ...(options.missedGraceMs === undefined ? {} : { missedGraceMs: options.missedGraceMs }),
  });
  const scheduler = new AutomationScheduler(service, {
    ...(options.tickIntervalMs === undefined ? {} : { intervalMs: options.tickIntervalMs }),
    ...(options.tick === undefined ? {} : { tick: options.tick }),
  });
  return { repos: options.repos, service, scheduler };
}

const RUNTIMES = new WeakMap<Database, AutomationRuntime>();

/**
 * Installs the process wiring used by the automation routes. The route table only receives the
 * database handle, so the executable session port and the test clock arrive through this registry.
 */
export function installAutomationRuntime(options: AutomationRuntimeOptions): InstalledAutomationRuntime {
  const db = options.repos.automations.db;
  const previous = RUNTIMES.get(db);
  const runtime = createAutomationRuntime(options);
  RUNTIMES.set(db, runtime);
  return {
    runtime,
    stop: () => runtime.scheduler.stop(),
    restore: () => {
      if (previous === undefined) RUNTIMES.delete(db);
      else RUNTIMES.set(db, previous);
    },
  };
}

/** Route accessor; without an explicit installation the session adapter resolves the runtime lazily. */
export function automationRuntimeFor(repos: PlatformRepositories): AutomationRuntime {
  const db = repos.automations.db;
  const installed = RUNTIMES.get(db);
  if (installed !== undefined) return installed;
  const runtime = createAutomationRuntime({
    repos,
    port: createSessionAutomationPort(repos),
  });
  RUNTIMES.set(db, runtime);
  return runtime;
}

/** Production entry point: installs the runtime and starts the one-second tick. */
export function startAutomationScheduler(
  options: AutomationRuntimeOptions & { readonly port: AutomationSessionPort },
): { readonly stop: () => Promise<void> } {
  const installed = installAutomationRuntime(options);
  installed.runtime.scheduler.start();
  return {
    stop: async () => {
      await installed.stop();
      installed.restore();
    },
  };
}
