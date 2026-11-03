import { execFileSync, spawn, spawnSync } from 'node:child_process';
import { accessSync, constants, existsSync, realpathSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { SandboxError } from '../instance/types.js';
import { verifyPeriRuntime } from '../instance/settings.js';
import { makeDirectories } from '../instance/workspace.js';
import { macTmpPath, removeMacTmp } from './policy.js';
import { srtRuntime } from './srt.js';
import type { DoctorOptions, Provider, ProviderHandle, ProviderOptions, ProviderStartOptions } from './provider.js';

const DEFAULT_STOP_TIMEOUT_MS = 15_000;

/**
 * Xcode 命令行工具目录。SRT 对 `/var/select` 的 readlink 仍会被拒绝（即使
 * allowRead 包含该路径），`xcode-select` 因此失败；显式设置 `DEVELOPER_DIR`
 * 可绕过 shim，让 git/python3/clang 直接找到工具链。
 */
function developerDir(): string | null {
  try {
    const target = realpathSync('/var/select/developer_dir');
    if (existsSync(target)) return target;
  } catch {
    // 没有 xcode-select 链接时回退到常见安装位置。
  }
  return existsSync('/Library/Developer/CommandLineTools') ? '/Library/Developer/CommandLineTools' : null;
}

/**
 * SRT 是 Bun 进程，Bun 会从 cwd 自动加载 `.env`。固定到沙箱根目录，
 * 避免把宿主服务的凭据与代理变量带进沙箱子进程。
 */
export function macSpawnOptions(options: ProviderStartOptions) {
  const developer = developerDir();
  return {
    detached: true,
    stdio: ['pipe', 'pipe', 'pipe'] as ['pipe', 'pipe', 'pipe'],
    cwd: options.sandboxRoot,
    env: {
      PATH: [
        dirname(process.execPath),
        '/opt/homebrew/bin',
        '/usr/local/bin',
        '/usr/bin',
        '/bin',
        '/opt/homebrew/sbin',
        '/usr/local/sbin',
        '/usr/sbin',
        '/sbin',
      ].join(':'),
      HOME: options.homePath,
      XDG_CONFIG_HOME: join(options.homePath, '.config'),
      TMPDIR: macTmpPath(options.sandboxId),
      CLAUDE_CODE_TMPDIR: join(macTmpPath(options.sandboxId), 'claude'),
      GIT_TEMPLATE_DIR: join(options.homePath, '.git-template'),
      LANG: 'en_US.UTF-8',
      ...(developer === null ? {} : { DEVELOPER_DIR: developer }),
    },
  };
}

/** macOS provider: SRT CLI in a detached process group, no registry. */
export class MacProvider implements Provider {
  readonly kind = 'macos-process-group' as const;
  private readonly stopTimeoutMs: number;

  constructor(options: ProviderOptions = {}) {
    this.stopTimeoutMs = options.stopTimeoutMs ?? DEFAULT_STOP_TIMEOUT_MS;
  }

  async doctor(options?: DoctorOptions): Promise<void> {
    if (process.platform !== 'darwin' || process.getuid?.() === 0)
      throw new SandboxError('unsupported_environment', 'requires an unprivileged macOS account');
    srtRuntime();
    try {
      accessSync('/usr/bin/sandbox-exec', constants.X_OK);
    } catch (error) {
      throw new SandboxError('unsupported_environment', 'macOS sandbox-exec unavailable', { cause: error });
    }
    if (options) verifyPeriRuntime(options.periPath, options.periSha256);
  }

  async start(options: ProviderStartOptions): Promise<ProviderHandle> {
    if (process.platform !== 'darwin')
      throw new SandboxError('unsupported_environment', 'macOS provider requires darwin');
    await this.doctor();
    const srt = srtRuntime();
    makeDirectories(options.sandboxesRoot, options.sandboxId);
    const child = spawn(
      process.execPath,
      [
        srt.cli,
        '--settings',
        options.policyPath,
        '--',
        options.periPath,
        'mcp-start',
        'workspace',
        '--stdio',
        '--workspace',
        options.workspacePath,
      ],
      macSpawnOptions(options),
    );
    if (!child.pid) throw new SandboxError('start_failed', 'failed to spawn SRT');
    const pid = child.pid;
    const started = this.started(pid);
    return {
      provider: this.kind,
      unit: null,
      process: child,
      stop: () => this.stop(pid, started, options),
    };
  }

  private groupAlive(pid: number): boolean {
    try {
      const groups = execFileSync('/bin/ps', ['-ax', '-o', 'pgid='], { encoding: 'utf8' });
      return groups.split(/\s+/).some((group) => Number(group) === pid);
    } catch (error) {
      throw new SandboxError('stop_incomplete', 'cannot inspect macOS process groups', { cause: error });
    }
  }

  private started(pid: number): string {
    const result = spawnSync('/bin/ps', ['-p', String(pid), '-o', 'lstart='], { encoding: 'utf8' });
    return result.status === 0 ? result.stdout.trim() : '';
  }

  private confirmLeader(pid: number, started: string, options: ProviderStartOptions): void {
    const command = spawnSync('/bin/ps', ['-p', String(pid), '-o', 'command='], { encoding: 'utf8' });
    if (
      !started ||
      this.started(pid) !== started ||
      command.status !== 0 ||
      !command.stdout.includes(srtRuntime().cli) ||
      !command.stdout.includes(options.policyPath)
    )
      throw new SandboxError('stop_incomplete', `cannot confirm owned macOS process group: ${pid}`);
  }

  private async stop(pid: number, started: string, options: ProviderStartOptions): Promise<void> {
    if (!Number.isSafeInteger(pid) || pid < 2) throw new SandboxError('stop_incomplete', 'invalid process group');
    for (const signal of ['SIGTERM', 'SIGKILL'] as const) {
      if (!this.groupAlive(pid)) {
        removeMacTmp(options.sandboxId);
        return;
      }
      this.confirmLeader(pid, started, options);
      try {
        process.kill(-pid, signal);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ESRCH' && this.groupAlive(pid))
          throw new SandboxError('stop_incomplete', String(error), { cause: error });
      }
      const until = Date.now() + this.stopTimeoutMs;
      while (Date.now() < until) {
        if (!this.groupAlive(pid)) {
          removeMacTmp(options.sandboxId);
          return;
        }
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
    }
    throw new SandboxError('stop_incomplete', `could not confirm stopped process group: ${pid}`);
  }
}
