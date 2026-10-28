import { spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { SandboxError } from '../instance/types.js';
import { verifyPeriRuntime } from '../instance/settings.js';
import { probeEnvironment, transientUnitProperties, verifySeccompHelper } from './doctor.js';
import { srtRuntime } from './srt.js';
import type { DoctorOptions, Provider, ProviderHandle, ProviderOptions, ProviderStartOptions } from './provider.js';

const DEFAULT_STOP_TIMEOUT_MS = 15_000;

async function systemctl(...args: string[]): Promise<{ code: number; text: string }> {
  const child = spawn('systemctl', ['--user', ...args], { stdio: ['ignore', 'pipe', 'pipe'] });
  let text = '';
  for (const stream of [child.stdout, child.stderr])
    stream.on('data', (chunk: Buffer) => {
      if (text.length < 32768) text += chunk.toString().slice(0, 32768 - text.length);
    });
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => child.kill('SIGKILL'), 10000);
    child.on('error', (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.on('close', (code) => {
      clearTimeout(timer);
      resolve({ code: code ?? 1, text });
    });
  });
}

async function command(program: string, args: string[]): Promise<{ status: number; output: string }> {
  const child = spawn(program, args, { stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  for (const stream of [child.stdout, child.stderr])
    stream.on('data', (data: Buffer) => {
      if (output.length < 32768) output += data.toString().slice(0, 32768 - output.length);
    });
  return new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (status) => resolve({ status: status ?? 1, output }));
  });
}

/** Linux provider: one `systemd-run` transient unit per sandbox instance. */
export class SystemdProvider implements Provider {
  readonly kind = 'systemd-user-unit' as const;
  private readonly prefix: string;
  private readonly stopTimeoutMs: number;

  constructor(options: ProviderOptions = {}) {
    this.prefix = options.unitPrefix ?? 'peri-sbx-';
    this.stopTimeoutMs = options.stopTimeoutMs ?? DEFAULT_STOP_TIMEOUT_MS;
  }

  unit(sandboxId: string, runId: string): string {
    return `${this.prefix}${sandboxId}-${runId}.service`;
  }

  private ownedUnit(unit: string): boolean {
    return new RegExp(`^${this.prefix}sbx_[a-f0-9]{32}-run_[a-f0-9]{24}\\.service$`).test(unit);
  }

  async doctor(options?: DoctorOptions): Promise<void> {
    if (process.platform !== 'linux' || process.getuid?.() === 0)
      throw new SandboxError('unsupported_environment', 'requires an unprivileged Linux systemd user manager');
    if (!existsSync('/sys/fs/cgroup/cgroup.controllers'))
      throw new SandboxError('unsupported_environment', 'cgroup v2 unavailable');
    const controllers = readFileSync('/sys/fs/cgroup/cgroup.controllers', 'utf8').split(/\s+/);
    if (!['cpu', 'memory', 'pids'].every((value) => controllers.includes(value)))
      throw new SandboxError('unsupported_environment', 'required cgroup controllers unavailable');
    const manager = await command('systemctl', ['--user', 'is-system-running']);
    if (manager.status && !['running', 'degraded'].includes(manager.output.trim()))
      throw new SandboxError('unsupported_environment', 'systemd user manager unavailable');
    verifySeccompHelper();
    srtRuntime();
    if (options) {
      verifyPeriRuntime(options.periPath, options.periSha256);
      await probeEnvironment(options, (unit) => this.stop(unit));
    }
  }

  async start(options: ProviderStartOptions): Promise<ProviderHandle> {
    await this.doctor();
    const srt = srtRuntime();
    const unit = this.unit(options.sandboxId, options.runId);
    const args = [
      '--user',
      '--pipe',
      '--wait',
      '--quiet',
      `--unit=${unit}`,
      `--working-directory=${options.sandboxRoot}`,
      ...transientUnitProperties(options.profile),
      '--',
      '/usr/bin/env',
      '-i',
      `PATH=${dirname(globalThis.process.execPath)}:/usr/bin:/bin`,
      `HOME=${options.homePath}`,
      `XDG_CONFIG_HOME=${join(options.homePath, '.config')}`,
      `GIT_TEMPLATE_DIR=${join(options.homePath, '.git-template')}`,
      'TMPDIR=/tmp',
      'CLAUDE_CODE_TMPDIR=/tmp/claude',
      'LANG=C.UTF-8',
      globalThis.process.execPath,
      srt.cli,
      '--settings',
      options.policyPath,
      '--',
      '/usr/bin/setsid',
      '--fork',
      '--wait',
      options.periPath,
      'mcp-start',
      'workspace',
      '--stdio',
      '--workspace',
      options.workspacePath,
    ];
    const child = spawn('systemd-run', args, {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: {
        PATH: '/usr/bin:/bin',
        XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR ?? '',
        DBUS_SESSION_BUS_ADDRESS: process.env.DBUS_SESSION_BUS_ADDRESS ?? '',
      },
    });
    if (!child.pid) throw new SandboxError('start_failed', 'failed to spawn systemd-run');
    return {
      provider: this.kind,
      unit,
      process: child,
      stop: () => this.stop(unit),
    };
  }

  async inspect(unit: string): Promise<{ inactive: boolean; cgroupEmpty: boolean }> {
    if (!this.ownedUnit(unit)) throw new SandboxError('conflict', 'unit outside owned namespace');
    const result = await systemctl(
      'show',
      unit,
      '-p',
      'LoadState',
      '-p',
      'ActiveState',
      '-p',
      'ControlGroup',
      '-p',
      'Slice',
    );
    const jobs = await systemctl('list-jobs', '--plain', '--no-legend');
    if (jobs.code || jobs.text.split('\n').some((line) => line.trim().split(/\s+/)[1] === unit))
      return { inactive: false, cgroupEmpty: false };
    const loaded = result.text.match(/^LoadState=(.*)$/m)?.[1];
    const active = result.text.match(/^ActiveState=(.*)$/m)?.[1];
    const group = result.text.match(/^ControlGroup=(.*)$/m)?.[1];
    if (result.code || !loaded || !active || group === undefined || !['loaded', 'not-found'].includes(loaded))
      return { inactive: false, cgroupEmpty: false };
    if (group) {
      if (!group.startsWith('/user.slice/') || !group.endsWith(`/${unit}`) || group.includes('..'))
        return { inactive: false, cgroupEmpty: false };
      const events = `/sys/fs/cgroup${group}/cgroup.events`;
      try {
        if (!existsSync(events) || !/^populated 0$/m.test(readFileSync(events, 'utf8')))
          return { inactive: false, cgroupEmpty: false };
      } catch {
        return { inactive: false, cgroupEmpty: false };
      }
    } else {
      if (loaded !== 'not-found' && result.text.match(/^Slice=(.*)$/m)?.[1] !== 'app.slice')
        return { inactive: false, cgroupEmpty: false };
      const slice = await systemctl('show', 'app.slice', '-p', 'ControlGroup', '--value');
      const parent = slice.text.trim();
      if (slice.code || !/^\/user\.slice\/user-\d+\.slice\/user@\d+\.service\/app\.slice$/.test(parent))
        return { inactive: false, cgroupEmpty: false };
      const expected = `/sys/fs/cgroup${parent}/${unit}`;
      if (existsSync(expected)) {
        try {
          if (!/^populated 0$/m.test(readFileSync(`${expected}/cgroup.events`, 'utf8')))
            return { inactive: false, cgroupEmpty: false };
        } catch {
          return { inactive: false, cgroupEmpty: false };
        }
      }
    }
    const pending = await systemctl('list-jobs', '--plain', '--no-legend');
    if (pending.code || pending.text.split('\n').some((line) => line.trim().split(/\s+/)[1] === unit))
      return { inactive: false, cgroupEmpty: false };
    return { inactive: loaded === 'not-found' || active === 'inactive' || active === 'failed', cgroupEmpty: true };
  }

  async stop(unit: string): Promise<void> {
    if (!this.ownedUnit(unit)) throw new SandboxError('conflict', 'unit outside owned namespace');
    await systemctl('stop', unit);
    const until = Date.now() + this.stopTimeoutMs;
    while (Date.now() < until) {
      const result = await this.inspect(unit);
      if (result.inactive && result.cgroupEmpty) return;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    await systemctl('kill', '--signal=SIGKILL', unit);
    const killUntil = Date.now() + this.stopTimeoutMs;
    while (Date.now() < killUntil) {
      const result = await this.inspect(unit);
      if (result.inactive && result.cgroupEmpty) return;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    throw new SandboxError('stop_incomplete', `could not confirm empty cgroup: ${unit}`);
  }

  async findActiveUnit(sandboxId: string): Promise<string | null> {
    const result = await systemctl(
      'list-units',
      '--all',
      '--plain',
      '--no-legend',
      `${this.prefix}${sandboxId}-*.service`,
    );
    if (result.code) throw new SandboxError('unsupported_environment', result.text);
    for (const line of result.text.split('\n')) {
      const fields = line.trim().split(/\s+/);
      const unit = fields[0];
      if (!unit || !unit.startsWith(`${this.prefix}${sandboxId}-`) || !this.ownedUnit(unit)) continue;
      if (fields[2] === 'active') return unit;
    }
    return null;
  }

  async findResidualUnits(): Promise<string[]> {
    const result = await systemctl('list-units', '--all', '--plain', '--no-legend', `${this.prefix}*.service`);
    if (result.code) throw new SandboxError('unsupported_environment', result.text);
    return result.text
      .split('\n')
      .map((line) => line.trim().split(/\s+/)[0])
      .filter((unit): unit is string => Boolean(unit) && this.ownedUnit(unit));
  }
}
