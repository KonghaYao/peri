import { spawn, execFileSync, spawnSync } from 'node:child_process';
import {
  accessSync,
  constants,
  mkdirSync,
  readdirSync,
  readFileSync,
  writeFileSync,
  unlinkSync,
  lstatSync,
} from 'node:fs';
import { dirname, join } from 'node:path';
import type { Config } from '../config.js';
import { verifyRuntime } from '../config.js';
import { ControlError } from '../control/types.js';
import { macTmpPath, makeDirectories, removeMacTmp, sandboxPath } from './policy.js';
import { srtRuntime } from './srt.js';
import type { Handle, Provider } from './provider.js';

export class MacProvider implements Provider {
  private readonly stopped = new Set<string>();
  constructor(private readonly config: Config) {}

  unit(id: string, generation: string): string {
    return `${this.config.provider.unitPrefix}${id}-${generation}.service`;
  }

  async doctor(): Promise<void> {
    if (process.platform !== 'darwin' || process.getuid?.() === 0)
      throw new ControlError('unsupported_environment', 'requires an unprivileged macOS account');
    verifyRuntime(this.config);
    srtRuntime();
    try {
      accessSync('/usr/bin/sandbox-exec', constants.X_OK);
    } catch {
      throw new ControlError('unsupported_environment', 'macOS sandbox-exec unavailable');
    }
  }

  private directory(): string {
    return join(this.config.controlRoot, 'processes');
  }

  private file(unit: string): string {
    if (!new RegExp(`^${this.config.provider.unitPrefix}sbx_[a-f0-9]{32}-run_[a-f0-9]{24}\\.service$`).test(unit))
      throw new ControlError('conflict', 'unit outside owned namespace');
    return join(this.directory(), unit);
  }

  async start(
    id: string,
    generation: string,
    policyPath: string,
    _profile: Config['profiles'][string],
  ): Promise<Handle> {
    await this.doctor();
    makeDirectories(this.config, id);
    const unit = this.unit(id, generation);
    const srt = srtRuntime();
    const base = sandboxPath(this.config, id);
    const home = join(base, 'home');
    const workspace = join(base, 'workspace');
    mkdirSync(this.directory(), { recursive: true, mode: 0o700 });
    const child = spawn(
      process.execPath,
      [
        srt.cli,
        '--settings',
        policyPath,
        '--',
        this.config.runtime.periPath,
        'mcp-start',
        'workspace',
        '--stdio',
        '--workspace',
        workspace,
      ],
      {
        detached: true,
        stdio: ['pipe', 'pipe', 'pipe'],
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
          HOME: home,
          XDG_CONFIG_HOME: join(home, '.config'),
          TMPDIR: macTmpPath(id),
          CLAUDE_CODE_TMPDIR: join(macTmpPath(id), 'claude'),
          LANG: 'en_US.UTF-8',
        },
      },
    );
    if (!child.pid) throw new ControlError('startup_failed', 'failed to spawn SRT');
    try {
      const started = this.started(child.pid);
      writeFileSync(this.file(unit), JSON.stringify({ pid: child.pid, started }), { flag: 'wx', mode: 0o600 });
    } catch (error) {
      try {
        process.kill(-child.pid, 'SIGKILL');
      } catch {
        /* child exited */
      }
      throw error;
    }
    return { unit, process: child };
  }

  private groupAlive(pid: number): boolean {
    try {
      const groups = execFileSync('/bin/ps', ['-ax', '-o', 'pgid='], { encoding: 'utf8' });
      return groups.split(/\s+/).some((group) => Number(group) === pid);
    } catch {
      throw new ControlError('stop_incomplete', 'cannot inspect macOS process groups');
    }
  }

  private started(pid: number): string {
    const result = spawnSync('/bin/ps', ['-p', String(pid), '-o', 'lstart='], { encoding: 'utf8' });
    return result.status === 0 ? result.stdout.trim() : '';
  }

  private confirmLeader(pid: number, started: string, unit: string): void {
    const command = spawnSync('/bin/ps', ['-p', String(pid), '-o', 'command='], { encoding: 'utf8' });
    const id = /sbx_[a-f0-9]{32}/.exec(unit)![0];
    const policyPath = join(this.config.controlRoot, 'policies', `${id}.json`);
    if (
      !started ||
      this.started(pid) !== started ||
      command.status !== 0 ||
      !command.stdout.includes(srtRuntime().cli) ||
      !command.stdout.includes(policyPath)
    )
      throw new ControlError('stop_incomplete', `cannot confirm owned macOS process group: ${unit}`);
  }

  async stop(unit: string): Promise<void> {
    const file = this.file(unit);
    if (this.stopped.has(unit)) return;
    if (lstatSync(file, { throwIfNoEntry: false })?.isSymbolicLink())
      throw new ControlError('stop_incomplete', 'symlinked process record');
    let pid: number;
    let started: string;
    try {
      const record = JSON.parse(readFileSync(file, 'utf8'));
      if (record.stopped === true) {
        this.stopped.add(unit);
        return;
      }
      ({ pid, started } = record);
    } catch {
      throw new ControlError('stop_incomplete', `missing process record: ${unit}`);
    }
    if (!Number.isSafeInteger(pid) || pid < 2) throw new ControlError('stop_incomplete', 'invalid process group');
    const finish = () => {
      removeMacTmp(/sbx_[a-f0-9]{32}/.exec(unit)![0]);
      writeFileSync(file, JSON.stringify({ stopped: true }), { mode: 0o600 });
      this.stopped.add(unit);
    };
    for (const signal of ['SIGTERM', 'SIGKILL'] as const) {
      if (!this.groupAlive(pid)) {
        finish();
        return;
      }
      this.confirmLeader(pid, started, unit);
      try {
        process.kill(-pid, signal);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ESRCH' && this.groupAlive(pid))
          throw new ControlError('stop_incomplete', String(error));
      }
      const until = Date.now() + this.config.limits.stopMs;
      while (Date.now() < until) {
        if (!this.groupAlive(pid)) {
          finish();
          return;
        }
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
    }
    throw new ControlError('stop_incomplete', `could not confirm stopped process group: ${unit}`);
  }

  async listOwned(): Promise<string[]> {
    mkdirSync(this.directory(), { recursive: true, mode: 0o700 });
    return readdirSync(this.directory()).filter((unit) => {
      try {
        this.file(unit);
        return true;
      } catch {
        return false;
      }
    });
  }

  pruneStopped(referencedUnits: Set<string>): void {
    for (const unit of readdirSync(this.directory())) {
      if (referencedUnits.has(unit)) continue;
      const file = this.file(unit);
      if (lstatSync(file).isSymbolicLink()) throw new ControlError('stop_incomplete', 'symlinked process record');
      if (JSON.parse(readFileSync(file, 'utf8')).stopped === true) unlinkSync(file);
    }
  }
}
