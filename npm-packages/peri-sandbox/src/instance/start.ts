import { randomBytes } from 'node:crypto';
import { chmodSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import type { ChildProcessWithoutNullStreams } from 'node:child_process';
import { PeriBackend } from '../runtime/backend.js';
import { DiagnosticLog } from '../runtime/diagnostics.js';
import { policyFor } from '../runtime/policy.js';
import { providerFor, type Provider, type ProviderHandle } from '../runtime/provider.js';
import { startHttpServer, type SandboxHttpServer, type SandboxHttpServerOptions } from '../server/http.js';
import { ManagedSandboxInstance } from './instance.js';
import { resolveOptions } from './options.js';
import { loadSettings, verifyPeriRuntime } from './settings.js';
import { SandboxError, type McpBackendPort, type SandboxInstance, type StartSandboxOptions } from './types.js';
import {
  acquireLock,
  checkFreeSpace,
  ensureSandboxesRoot,
  makeDirectories,
  resolveToken,
  type SandboxLock,
} from './workspace.js';
import { withTimeout } from '../util/timeout.js';

export interface BackendFactoryInput {
  sandboxId: string;
  runId: string;
  process: ChildProcessWithoutNullStreams;
  bashCalls: number;
  sandboxRoot: string;
  onExit: (error: Error) => void;
}

/** Internal seams for unit tests; not exported from the package barrel. */
export interface StartSandboxDependencies {
  provider?: Provider;
  backendFactory?: (input: BackendFactoryInput) => McpBackendPort | Promise<McpBackendPort>;
  httpFactory?: (options: SandboxHttpServerOptions) => Promise<SandboxHttpServer>;
  verifyRuntime?: (periPath: string, expectedSha256: string) => void;
}

function abortError(): SandboxError {
  return new SandboxError('start_cancelled', 'Sandbox startup was cancelled.');
}

function throwIfAborted(signal: AbortSignal | undefined): void {
  if (signal?.aborted) throw abortError();
}

async function raceAbort<T>(promise: Promise<T>, signal: AbortSignal | undefined): Promise<T> {
  if (!signal) return promise;
  if (signal.aborted) throw abortError();
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(abortError());
    signal.addEventListener('abort', onAbort, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener('abort', onAbort);
        resolve(value);
      },
      (error) => {
        signal.removeEventListener('abort', onAbort);
        reject(error);
      },
    );
  });
}

function asSandboxError(error: unknown, fallback: 'start_failed' | 'stop_incomplete'): SandboxError {
  if (error instanceof SandboxError) return error;
  return new SandboxError(fallback, error instanceof Error ? error.message : String(error), { cause: error });
}

function writePolicy(sandboxRoot: string, json: string): string {
  const path = join(sandboxRoot, 'policy.json');
  writeFileSync(path, json, { mode: 0o600 });
  chmodSync(path, 0o600);
  return path;
}

function defaultBackendFactory(input: BackendFactoryInput): Promise<McpBackendPort> {
  const backend = new PeriBackend({
    id: input.sandboxId,
    runId: input.runId,
    process: input.process,
    bashLimit: input.bashCalls,
    diagnostics: new DiagnosticLog(input.sandboxRoot),
    onExit: input.onExit,
  });
  return backend.ready().then(() => backend);
}

/**
 * Start one sandbox instance in the caller's process.
 *
 * The workspace lock serializes instances sharing `sandboxId`; the HTTP MCP
 * endpoint binds an ephemeral port unless `port` is explicit.
 */
export function startSandbox(options: StartSandboxOptions = {}): Promise<SandboxInstance> {
  return startSandboxWith(options, {});
}

export async function startSandboxWith(
  options: StartSandboxOptions,
  dependencies: StartSandboxDependencies,
): Promise<SandboxInstance> {
  const settings = loadSettings(options.settingsPath);
  const resolved = resolveOptions(options, settings);
  (dependencies.verifyRuntime ?? verifyPeriRuntime)(resolved.periPath, resolved.periSha256);

  ensureSandboxesRoot(resolved.sandboxesRoot);
  checkFreeSpace(resolved.sandboxesRoot, resolved.minFreeBytes);
  makeDirectories(resolved.sandboxesRoot, resolved.sandboxId);

  const lock: SandboxLock = acquireLock(resolved.sandboxRoot);
  const provider =
    dependencies.provider ??
    providerFor(process.platform, {
      unitPrefix: resolved.unitPrefix,
      stopTimeoutMs: resolved.stopTimeoutMs,
    });

  let http: SandboxHttpServer | undefined;
  let handle: ProviderHandle | undefined;
  let backend: McpBackendPort | undefined;

  const unwind = async (primary: unknown): Promise<SandboxError> => {
    const failures: unknown[] = [];
    if (http) await http.close().catch((error) => failures.push(error));
    if (backend) await backend.close().catch(() => undefined);
    if (handle) {
      try {
        await withTimeout(
          handle.stop(),
          resolved.stopTimeoutMs,
          () => new SandboxError('stop_incomplete', 'Sandbox stop timed out during startup cleanup.'),
        );
      } catch (error) {
        failures.push(error);
      }
    }
    if (failures.length === 0) {
      try {
        lock.release();
      } catch (error) {
        failures.push(error);
      }
    }
    const mapped = asSandboxError(primary, 'start_failed');
    if (failures.length > 0)
      return new SandboxError('stop_incomplete', 'Sandbox startup failed and cleanup was not confirmed.', {
        cause: new AggregateError([mapped, ...failures]),
      });
    return mapped;
  };

  try {
    if (provider.findActiveUnit) {
      const active = await provider.findActiveUnit(resolved.sandboxId);
      if (active) throw new SandboxError('conflict', `Sandbox unit is still active: ${active}. Stop it manually.`);
    }
    throwIfAborted(resolved.signal);

    const { token, tokenFile } = resolveToken({
      sandboxRoot: resolved.sandboxRoot,
      token: resolved.token,
      tokenFile: resolved.tokenFile,
    });
    const policy = policyFor({
      sandboxesRoot: resolved.sandboxesRoot,
      sandboxId: resolved.sandboxId,
      profile: resolved.profile,
      periPath: resolved.periPath,
    });
    const policyPath = writePolicy(resolved.sandboxRoot, policy.json);
    const runId = `run_${randomBytes(12).toString('hex')}`;

    let instance: ManagedSandboxInstance | undefined;
    const httpFactory = dependencies.httpFactory ?? startHttpServer;
    http = await httpFactory({
      sandboxId: resolved.sandboxId,
      host: resolved.host,
      port: resolved.port,
      token,
      state: () => instance?.state ?? 'starting',
      backend: () => backend ?? null,
    });
    throwIfAborted(resolved.signal);

    const pendingHandle = provider.start({
      sandboxId: resolved.sandboxId,
      runId,
      sandboxesRoot: resolved.sandboxesRoot,
      sandboxRoot: resolved.sandboxRoot,
      workspacePath: resolved.workspacePath,
      homePath: resolved.homePath,
      policyPath,
      profile: resolved.profile,
      periPath: resolved.periPath,
    });
    try {
      handle = await raceAbort(pendingHandle, resolved.signal);
    } catch (error) {
      if (resolved.signal?.aborted) void pendingHandle.then((late) => late.stop()).catch(() => undefined);
      throw error;
    }
    if (handle.process.pid) lock.update(handle.process.pid);

    const factory = dependencies.backendFactory ?? defaultBackendFactory;
    backend = await withTimeout(
      Promise.resolve(
        factory({
          sandboxId: resolved.sandboxId,
          runId,
          process: handle.process,
          bashCalls: resolved.bashCalls,
          sandboxRoot: resolved.sandboxRoot,
          onExit: (error) => instance?.markBackendExited(error),
        }),
      ),
      resolved.startupTimeoutMs,
      () => new SandboxError('start_failed', 'Sandbox backend did not become ready before the startup timeout.'),
    );
    throwIfAborted(resolved.signal);

    instance = new ManagedSandboxInstance({
      sandboxId: resolved.sandboxId,
      runId,
      token,
      tokenFile,
      sandboxRoot: resolved.sandboxRoot,
      sandboxesRoot: resolved.sandboxesRoot,
      workspacePath: resolved.workspacePath,
      runtime: {
        provider: handle.provider,
        unit: handle.unit,
        pid: handle.process.pid ?? null,
      },
      http,
      backend,
      lock,
      stopTimeoutMs: resolved.stopTimeoutMs,
      stop: handle.stop,
    });
    return instance;
  } catch (error) {
    throw await unwind(error);
  }
}
