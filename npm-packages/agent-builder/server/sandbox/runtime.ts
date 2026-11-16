import { createHash } from 'node:crypto';
import { chmodSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import type { PeriSandboxMcpRuntime } from '@peri-code/rivet/sandbox';
import { sandboxStartupError } from '../config-error';
import type { ServerEnv } from '../env';
import { ensurePrivateDirectory } from '../filesystem';

export interface SandboxRuntimeHandle {
  readonly runtime: PeriSandboxMcpRuntime;
  readonly settingsPath: string;
  /** True when the app created the settings file; it is kept for the next restart. */
  readonly generatedSettings: boolean;
}

/**
 * Generates a private peri-sandbox settings file when the operator did not provide one. The file
 * carries the binary path and hash, never credentials; it is 0600 inside a 0700 directory.
 */
function ensureGeneratedSettings(env: ServerEnv, defaultProfile: (periPath: string) => unknown): string {
  const sandbox = env.sandbox;
  const directory = path.join(sandbox.dataDir, 'sandbox');
  ensurePrivateDirectory(directory);
  const file = path.join(directory, 'settings.json');
  const periPath = sandbox.periPath;
  if (periPath === undefined) throw new Error('Sandbox peri binary is missing');
  const content = `${JSON.stringify(
    {
      sandboxesRoot: sandbox.sandboxesRoot,
      periPath,
      periSha256: createHash('sha256').update(readFileSync(periPath)).digest('hex'),
      defaultProfile: sandbox.profileName,
      profiles: { [sandbox.profileName]: defaultProfile(periPath) },
      startupTimeoutMs: sandbox.startupTimeoutMs,
      stopTimeoutMs: sandbox.stopTimeoutMs,
    },
    null,
    2,
  )}\n`;
  if (!existsSync(file) || readFileSync(file, 'utf8') !== content) {
    writeFileSync(file, content, { mode: 0o600 });
  }
  chmodSync(file, 0o600);
  return file;
}

/**
 * Only enabled sandbox deployments import peri-sandbox: pure Chat installs must not pay for the
 * native sandbox dependency or fail when its platform package is unavailable.
 */
export async function createSandboxRuntime(env: ServerEnv): Promise<SandboxRuntimeHandle> {
  const sandbox = env.sandbox;
  if (!sandbox.enabled) throw new Error('Sandbox runtime requested while sandbox is disabled');
  if (sandbox.mode !== 'ready' || sandbox.periPath === undefined) {
    // 环境加载已把可行动的部署原因收敛成 SandboxReason；只把白名单原因带出启动边界。
    const reason = sandbox.reasons[0];
    const typed = reason === undefined ? null : sandboxStartupError(reason);
    if (typed !== null) throw typed;
    // 缺失原因属于内部不变量破坏，仍走 internal_error，不猜测一个看似可信的 code。
    throw new Error('Sandbox is misconfigured without a recognized reason');
  }
  if (sandbox.sandboxesRoot === undefined) throw new Error('Sandbox root is not configured');

  const periSandbox = await import('peri-sandbox');
  const settingsPath = sandbox.settingsPath ?? ensureGeneratedSettings(env, periSandbox.defaultProfile);
  ensurePrivateDirectory(sandbox.sandboxesRoot);

  const { createPeriSandboxMcpRuntime } = await import('@peri-code/rivet/sandbox');
  const runtime = createPeriSandboxMcpRuntime({
    machineId: env.machineId,
    workspaces: [{ uri: env.workspaceUri, path: env.workspacePath }],
    settingsPath,
    sandboxesRoot: sandbox.sandboxesRoot,
    profileName: sandbox.profileName,
    startupTimeoutMs: sandbox.startupTimeoutMs,
    stopTimeoutMs: sandbox.stopTimeoutMs,
    // Local single-user installs should not refuse to start because the host disk is nearly full;
    // peri-sandbox reports its own disk failures when the environment actually starts.
    minFreeBytes: 0,
  });
  return { runtime, settingsPath, generatedSettings: sandbox.settingsPath === undefined };
}
