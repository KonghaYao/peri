import { startSandbox } from './instance/start.js';
import type { SandboxInstance, SandboxProfile, StartSandboxOptions } from './instance/types.js';
import { deleteSandboxData } from './instance/workspace.js';

/**
 * Structural mirror of the SDK sandbox adapter contract.
 *
 * `peri-sandbox` intentionally does not import `@peri-code/sdk`: the shapes are
 * kept structurally compatible and verified by the SDK test suite.
 */

/**
 * Structural mirror of the SDK workspace value object.
 *
 * `runtimePath` is the root the agent/session sees; `hostPath` is the local
 * host path. A local sandbox uses the same path for both.
 */
interface WorkspaceRef {
  id: string;
  hostPath?: string;
  runtimePath: string;
}

interface SandboxAdapterInstance {
  readonly sandboxId: string;
  readonly workspace: WorkspaceRef;
  readonly mcpServer: { type: 'http'; url: string; headers: Record<string, string> };
  close(): Promise<void>;
}

interface SandboxAdapterStartOptions<TOptions> {
  sandboxId: string;
  signal?: AbortSignal;
  options?: TOptions;
}

interface SandboxAdapter<TOptions> {
  start(options: SandboxAdapterStartOptions<TOptions>): Promise<SandboxAdapterInstance>;
  deleteData?(sandboxId: string): Promise<void>;
}

/** Deployment-level settings shared by every sandbox from one adapter. */
export interface PeriSandboxAdapterOptions {
  /** Settings file used when an explicit `sandboxesRoot` is absent. */
  settingsPath?: string;
  /** Deployment root; overrides the settings file value. */
  sandboxesRoot?: string;
}

/** Per-sandbox options forwarded to `startSandbox()`. */
export interface PeriSandboxStartOptions {
  profileName?: string;
  profile?: SandboxProfile;
  port?: number;
  bashCalls?: number;
  minFreeBytes?: number;
  startupTimeoutMs?: number;
  stopTimeoutMs?: number;
}

/** Internal seams for adapter unit tests; not exported from the package barrel. */
export interface PeriSandboxAdapterDependencies {
  startSandbox?: (options: StartSandboxOptions) => Promise<SandboxInstance>;
  deleteSandboxData?: (options: { sandboxId: string; sandboxesRoot?: string; settingsPath?: string }) => Promise<void>;
}

function startOptions(
  options: PeriSandboxAdapterOptions,
  request: SandboxAdapterStartOptions<PeriSandboxStartOptions>,
): StartSandboxOptions {
  const start = request.options ?? {};
  return {
    sandboxId: request.sandboxId,
    ...(options.settingsPath === undefined ? {} : { settingsPath: options.settingsPath }),
    ...(options.sandboxesRoot === undefined ? {} : { sandboxesRoot: options.sandboxesRoot }),
    ...(start.profileName === undefined ? {} : { profileName: start.profileName }),
    ...(start.profile === undefined ? {} : { profile: start.profile }),
    ...(start.port === undefined ? {} : { port: start.port }),
    ...(start.bashCalls === undefined ? {} : { bashCalls: start.bashCalls }),
    ...(start.minFreeBytes === undefined ? {} : { minFreeBytes: start.minFreeBytes }),
    ...(start.startupTimeoutMs === undefined ? {} : { startupTimeoutMs: start.startupTimeoutMs }),
    ...(start.stopTimeoutMs === undefined ? {} : { stopTimeoutMs: start.stopTimeoutMs }),
    ...(request.signal === undefined ? {} : { signal: request.signal }),
  };
}

/**
 * Build an SDK-compatible adapter around the single-instance `peri-sandbox` API.
 *
 * The returned instance exposes its workspace as
 * `{ id: sandboxId, hostPath, runtimePath }`; a local sandbox uses the same
 * path for host and runtime. The bearer token is exposed only through
 * `mcpServer.headers.Authorization`. `close()` stops the sandbox without
 * deleting its workspace, and `deleteData()` removes persisted data by id.
 */
export function createPeriSandboxAdapter(
  options: PeriSandboxAdapterOptions = {},
): SandboxAdapter<PeriSandboxStartOptions> {
  return createPeriSandboxAdapterWith(options, {});
}

/** @internal Implementation seam used by adapter tests with fake lifecycle calls. */
export function createPeriSandboxAdapterWith(
  options: PeriSandboxAdapterOptions,
  dependencies: PeriSandboxAdapterDependencies,
): SandboxAdapter<PeriSandboxStartOptions> {
  const start = dependencies.startSandbox ?? startSandbox;
  const remove = dependencies.deleteSandboxData ?? deleteSandboxData;

  return {
    async start(request) {
      const instance = await start(startOptions(options, request));
      return {
        sandboxId: instance.id,
        workspace: {
          id: instance.id,
          hostPath: instance.workspacePath,
          runtimePath: instance.workspacePath,
        },
        mcpServer: {
          type: 'http',
          url: instance.mcpUrl,
          headers: { Authorization: `Bearer ${instance.token}` },
        },
        close: () => instance.close(),
      };
    },
    async deleteData(sandboxId) {
      await remove({
        sandboxId,
        ...(options.sandboxesRoot === undefined ? {} : { sandboxesRoot: options.sandboxesRoot }),
        ...(options.settingsPath === undefined ? {} : { settingsPath: options.settingsPath }),
      });
    },
  };
}
