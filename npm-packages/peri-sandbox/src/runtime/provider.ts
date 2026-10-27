import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import type { Config } from '../config.js';
import { verifyRuntime } from '../config.js';
import { sandboxPath } from './policy.js';
import { ControlError } from '../control/types.js';
import { probeEnvironment, transientUnitProperties, verifySeccompHelper } from './doctor.js';
import { srtRuntime } from './srt.js';

export interface Handle {
  unit: string;
  process: ChildProcessWithoutNullStreams;
}

export interface Provider {
  unit(id: string, generation: string): string;
  doctor(requireApiActive?: boolean): Promise<void>;
  start(id: string, generation: string, policyPath: string, profile: Config['profiles'][string]): Promise<Handle>;
  stop(unit: string): Promise<void>;
  listOwned(): Promise<string[]>;
  pruneStopped?(referencedUnits: Set<string>): void;
}

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

export class SystemdProvider {
  constructor(private readonly config: Config) {}
  unit(id: string, generation: string): string {
    return `${this.config.provider.unitPrefix}${id}-${generation}.service`;
  }
  async doctor(requireApiActive = false): Promise<void> {
    await probeEnvironment(this.config, requireApiActive, (unit) => this.stop(unit));
  }
  async start(
    id: string,
    generation: string,
    policyPath: string,
    profile: Config['profiles'][string],
  ): Promise<Handle> {
    verifyRuntime(this.config);
    verifySeccompHelper();
    const srt = srtRuntime();
    const unit = this.unit(id, generation),
      base = sandboxPath(this.config, id);
    const home = join(base, 'home'),
      workspace = join(base, 'workspace');
    const args = [
      '--user',
      '--pipe',
      '--wait',
      '--quiet',
      `--unit=${unit}`,
      ...transientUnitProperties(this.config, profile, true),
      '--',
      '/usr/bin/env',
      '-i',
      `PATH=${dirname(globalThis.process.execPath)}:/usr/bin:/bin`,
      `HOME=${home}`,
      `XDG_CONFIG_HOME=${join(home, '.config')}`,
      'TMPDIR=/tmp',
      'CLAUDE_CODE_TMPDIR=/tmp/claude',
      'LANG=C.UTF-8',
      globalThis.process.execPath,
      srt.cli,
      '--settings',
      policyPath,
      '--',
      '/usr/bin/setsid',
      '--fork',
      '--wait',
      this.config.runtime.periPath,
      'mcp-start',
      'workspace',
      '--stdio',
      '--workspace',
      workspace,
    ];
    const process = spawn('systemd-run', args, {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: {
        PATH: '/usr/bin:/bin',
        XDG_RUNTIME_DIR: processEnv('XDG_RUNTIME_DIR'),
        DBUS_SESSION_BUS_ADDRESS: processEnv('DBUS_SESSION_BUS_ADDRESS'),
      },
    });
    return { unit, process };
  }
  async inspect(unit: string): Promise<{ inactive: boolean; cgroupEmpty: boolean }> {
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
    if (!new RegExp(`^${this.config.provider.unitPrefix}sbx_[a-f0-9]{32}-run_[a-f0-9]{24}\\.service$`).test(unit))
      throw new ControlError('conflict', 'unit outside owned namespace');
    await systemctl('stop', unit);
    const until = Date.now() + this.config.limits.stopMs;
    while (Date.now() < until) {
      const result = await this.inspect(unit);
      if (result.inactive && result.cgroupEmpty) return;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    await systemctl('kill', '--signal=SIGKILL', unit);
    const killUntil = Date.now() + this.config.limits.stopMs;
    while (Date.now() < killUntil) {
      const result = await this.inspect(unit);
      if (result.inactive && result.cgroupEmpty) return;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    throw new ControlError('stop_incomplete', `could not confirm empty cgroup: ${unit}`);
  }
  async listOwned(): Promise<string[]> {
    const result = await systemctl(
      'list-units',
      '--all',
      '--plain',
      '--no-legend',
      `${this.config.provider.unitPrefix}*.service`,
    );
    if (result.code) throw new ControlError('unsupported_environment', result.text);
    return result.text
      .split('\n')
      .map((line) => line.trim().split(/\s+/)[0])
      .filter((unit) => unit.startsWith(this.config.provider.unitPrefix) && unit.endsWith('.service'));
  }
}

function processEnv(key: string): string {
  return process.env[key] ?? '';
}
