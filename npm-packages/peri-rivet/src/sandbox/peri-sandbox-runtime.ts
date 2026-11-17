import { lstatSync, realpathSync, statSync } from 'node:fs';
import { isAbsolute, relative, resolve } from 'node:path';
import {
  type SandboxInstance,
  type SandboxProfile,
  type StartSandboxOptions,
  startSandbox,
} from 'peri-sandbox';
import { PeriError } from '../contracts/errors.js';
import type { SandboxMcpRuntimePort } from '../rivet/sandbox-mcp-actor.js';
import { parseMachineId, parseWorkspaceUri, type WorkspaceUri } from '../rivet/workspace-uri.js';

/** 一个 host 明确授权的 workspace：URI 与本地真实目录必须指向同一位置。 */
export interface SandboxWorkspaceGrant {
  /** canonical `peri+sandbox://` URI，必须与 actor key 完全一致。 */
  readonly uri: string;
  /** 已存在的 0700 真实目录；不能是 symlink，也不能落在 sandboxesRoot 内。 */
  readonly path: string;
}

export interface PeriSandboxMcpRuntimeOptions {
  /** 可信部署身份；授权的 URI 必须与它同机。 */
  readonly machineId: string;
  readonly workspaces: readonly SandboxWorkspaceGrant[];
  readonly settingsPath?: string;
  readonly sandboxesRoot?: string;
  readonly profileName?: string;
  readonly profile?: SandboxProfile;
  readonly startupTimeoutMs?: number;
  readonly stopTimeoutMs?: number;
  readonly minFreeBytes?: number;
  readonly bashCalls?: number;
}

/** 宿主进程级清理入口；actor 的 closeAll 只关闭单个 workspace。 */
export interface PeriSandboxMcpRuntime extends SandboxMcpRuntimePort {
  closeAll(): Promise<void>;
}

/** Internal seams for adapter unit tests; not exported from the package barrel. */
export interface PeriSandboxMcpRuntimeDependencies {
  startSandbox?: (options: StartSandboxOptions) => Promise<SandboxInstance>;
}

function invalid(message: string, reason: string): never {
  throw new PeriError({
    code: 'invalid_input',
    phase: 'input',
    message,
    details: { reason },
  });
}

function withinOrEqual(candidate: string, root: string): boolean {
  const rel = relative(root, candidate);
  return rel === '' || (!rel.startsWith('..') && !isAbsolute(rel));
}

function canonicalRoot(path: string | undefined): string | undefined {
  if (path === undefined) {
    return undefined;
  }
  try {
    return realpathSync(path);
  } catch {
    return resolve(path);
  }
}

/**
 * 把 URI 绑定到 host 认可的真实目录。
 *
 * 只做精确 realpath 比较，不用前缀匹配；因此同机不同目录、symlink 别名或
 * `sandboxesRoot` 内的路径都不会被“看起来像”授权。
 */
function prepareGrants(options: PeriSandboxMcpRuntimeOptions): {
  readonly grants: ReadonlyMap<WorkspaceUri, string>;
  readonly sandboxesRoot: string | undefined;
} {
  const machineId = parseMachineId(options.machineId);
  const sandboxesRoot = canonicalRoot(options.sandboxesRoot);
  const grants = new Map<WorkspaceUri, string>();
  const paths = new Set<string>();

  for (const grant of options.workspaces) {
    const parsed = parseWorkspaceUri(grant.uri);
    if (parsed.machineId !== machineId) {
      throw new PeriError({
        code: 'invalid_identity',
        phase: 'input',
        message: 'authorized workspace belongs to another machine',
        details: { reason: 'machine_mismatch' },
      });
    }
    if (grants.has(parsed.uri)) {
      invalid('authorized workspace is duplicated', 'duplicate_workspace');
    }
    if (!isAbsolute(grant.path) || resolve(grant.path) !== grant.path) {
      invalid('workspace path must be a normalized absolute path', 'non_canonical_path');
    }
    const entry = lstatSync(grant.path, { throwIfNoEntry: false });
    if (!entry || entry.isSymbolicLink() || !statSync(grant.path).isDirectory()) {
      invalid('workspace path must be an existing real directory', 'workspace_missing');
    }
    if (entry.mode & 0o077) {
      invalid('workspace path must be private (0700)', 'workspace_not_private');
    }
    const hostPath = realpathSync(grant.path);
    if (hostPath === '/' || hostPath !== grant.path) {
      invalid('workspace path must be a canonical real directory', 'workspace_non_canonical');
    }
    if (paths.has(hostPath)) {
      invalid('workspace path is duplicated', 'duplicate_path');
    }
    if (
      sandboxesRoot !== undefined &&
      (withinOrEqual(hostPath, sandboxesRoot) || withinOrEqual(sandboxesRoot, hostPath))
    ) {
      invalid('workspace path must not overlap sandboxesRoot', 'workspace_overlap');
    }
    // The URI path is a literal locator, not an alias: resolving symlinks here
    // would let one grant authorize a different spelling of the same directory.
    if (parsed.runtimePath !== hostPath) {
      invalid('workspace URI path does not match the authorized directory', 'workspace_uri_mismatch');
    }
    grants.set(parsed.uri, hostPath);
    paths.add(hostPath);
  }
  return { grants, sandboxesRoot };
}

function startOptions(hostPath: string, options: PeriSandboxMcpRuntimeOptions): StartSandboxOptions {
  // `uri` is deliberately not passed to peri-sandbox. Its internal `sbx_...`
  // handle is only a process/state-directory resource id; deriving it from the
  // URI would create a second, persistent identity for the same workspace.
  return {
    workspacePath: hostPath,
    host: '127.0.0.1',
    port: 0,
    ...(options.settingsPath === undefined ? {} : { settingsPath: options.settingsPath }),
    ...(options.sandboxesRoot === undefined ? {} : { sandboxesRoot: options.sandboxesRoot }),
    ...(options.profileName === undefined ? {} : { profileName: options.profileName }),
    ...(options.profile === undefined ? {} : { profile: options.profile }),
    ...(options.startupTimeoutMs === undefined ? {} : { startupTimeoutMs: options.startupTimeoutMs }),
    ...(options.stopTimeoutMs === undefined ? {} : { stopTimeoutMs: options.stopTimeoutMs }),
    ...(options.minFreeBytes === undefined ? {} : { minFreeBytes: options.minFreeBytes }),
    ...(options.bashCalls === undefined ? {} : { bashCalls: options.bashCalls }),
  };
}

function unavailableResponse(status: number, code: string, message: string): Response {
  return new Response(JSON.stringify({ error: { code, message } }), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

function requestHeaders(request: Request, innerToken: string): Headers {
  const headers = new Headers(request.headers);
  for (const name of [
    'authorization',
    'connection',
    'content-length',
    'cookie',
    'host',
    'keep-alive',
    'proxy-authenticate',
    'proxy-authorization',
    'te',
    'trailer',
    'transfer-encoding',
    'upgrade',
    'x-peri-sandbox-token',
    'x-rivet-token',
  ]) {
    headers.delete(name);
  }
  headers.set('authorization', `Bearer ${innerToken}`);
  return headers;
}

/**
 * `@peri-code/rivet/sandbox` 的真实宿主适配器。
 *
 * 它只依赖 `peri-sandbox` 的单实例 API：环境、内部 MCP、token 与端口都由
 * `startSandbox` 建立；本模块负责 URI→真实目录授权、进程级实例表和 HTTP 转接。
 */
export function createPeriSandboxMcpRuntimeWith(
  options: PeriSandboxMcpRuntimeOptions,
  dependencies: PeriSandboxMcpRuntimeDependencies = {},
): PeriSandboxMcpRuntime {
  const { grants } = prepareGrants(options);
  const start = dependencies.startSandbox ?? startSandbox;
  const instances = new Map<WorkspaceUri, SandboxInstance>();
  const starting = new Map<WorkspaceUri, Promise<SandboxInstance>>();

  function grantFor(workspaceUri: WorkspaceUri): string | undefined {
    return grants.get(workspaceUri);
  }

  async function closeInstance(workspaceUri: WorkspaceUri): Promise<void> {
    const pending = starting.get(workspaceUri);
    if (pending !== undefined) {
      await pending.catch(() => undefined);
    }
    const instance = instances.get(workspaceUri);
    if (instance === undefined) {
      return;
    }
    // The handle is process-local and has no business identity; its managed
    // state directory is disposable once the instance is gone. The explicit
    // workspace is outside `sandboxesRoot` and is never deleted here.
    await instance.close({ deleteData: true });
    instances.delete(workspaceUri);
  }

  return {
    async start(workspaceUri: string): Promise<unknown> {
      const uri = workspaceUri as WorkspaceUri;
      const hostPath = grantFor(uri);
      if (hostPath === undefined) {
        throw new PeriError({
          code: 'invalid_identity',
          phase: 'input',
          message: 'workspace is not authorized',
          details: { reason: 'workspace_not_authorized' },
        });
      }
      const existing = instances.get(uri);
      if (existing !== undefined && existing.state === 'ready') {
        return { workspaceUri: uri, state: existing.state };
      }
      const pending = starting.get(uri);
      if (pending !== undefined) {
        await pending;
        return { workspaceUri: uri, state: 'ready' };
      }
      const task = (async (): Promise<SandboxInstance> => {
        if (existing !== undefined) {
          await existing.close({ deleteData: true });
          instances.delete(uri);
        }
        const instance = await start(startOptions(hostPath, options));
        instances.set(uri, instance);
        return instance;
      })();
      starting.set(uri, task);
      try {
        await task;
        return { workspaceUri: uri, state: 'ready' };
      } finally {
        starting.delete(uri);
      }
    },

    async close(workspaceUri: string): Promise<void> {
      await closeInstance(workspaceUri as WorkspaceUri);
    },

    stateOf(workspaceUri: string): string | undefined {
      return instances.get(workspaceUri as WorkspaceUri)?.state;
    },

    async fetchMcp(workspaceUri: WorkspaceUri, request: Request): Promise<Response> {
      if (grantFor(workspaceUri) === undefined) {
        return unavailableResponse(403, 'invalid_identity', 'workspace is not authorized');
      }
      const instance = instances.get(workspaceUri);
      if (instance === undefined) {
        return unavailableResponse(503, 'sandbox_not_held', 'sandbox is not running');
      }
      if (instance.state !== 'ready') {
        return unavailableResponse(503, 'sandbox_not_ready', 'sandbox is not ready');
      }
      const bodyless = request.method === 'GET' || request.method === 'HEAD';
      const init: RequestInit & { duplex?: 'half' } = {
        method: request.method,
        headers: requestHeaders(request, instance.token),
        signal: request.signal,
        redirect: 'manual',
        ...(bodyless || request.body === null ? {} : { body: request.body, duplex: 'half' }),
      };
      return await fetch(instance.mcpUrl, init);
    },

    async closeAll(): Promise<void> {
      const failures: unknown[] = [];
      await Promise.allSettled([...starting.values()]);
      for (const workspaceUri of [...instances.keys()]) {
        try {
          await closeInstance(workspaceUri);
        } catch (error) {
          failures.push(error);
        }
      }
      if (failures.length > 0) {
        throw new AggregateError(failures, 'some sandbox instances did not close');
      }
    },
  };
}

/** 生产入口：使用 `peri-sandbox` 的真实环境与 MCP。 */
export function createPeriSandboxMcpRuntime(options: PeriSandboxMcpRuntimeOptions): PeriSandboxMcpRuntime {
  return createPeriSandboxMcpRuntimeWith(options);
}
