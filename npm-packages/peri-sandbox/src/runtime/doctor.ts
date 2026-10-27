import { randomBytes } from 'node:crypto';
import { spawn } from 'node:child_process';
import { accessSync, constants, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import type { Config } from '../config.js';
import { makeDirectories, policyFor, sandboxPath } from './policy.js';
import { ControlError } from '../control/types.js';
import { srtRuntime } from './srt.js';

async function command(program: string, args: string[]): Promise<{ status: number; output: string }> {
  const child = spawn(program, args, { stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  for (const stream of [child.stdout, child.stderr])
    stream.on('data', (data: Buffer) => {
      if (output.length < 32768) output += data.toString().slice(0, 32768 - output.length);
    });
  return new Promise((resolveResult, reject) => {
    child.once('error', reject);
    child.once('close', (status) => resolveResult({ status: status ?? 1, output }));
  });
}

const runtimeProbe = `
const fs = require('node:fs');
const net = require('node:net');
const [socketPath, secretPath, tmpMarker, workspace] = process.argv.slice(1);
try {
  if (!fs.statSync(socketPath).isSocket()) throw Error('workspace socket not visible');
  for (const path of [secretPath, tmpMarker]) {
    try { fs.readFileSync(path); throw Error('protected path readable: ' + path); }
    catch (error) { if (!['EACCES', 'EPERM', 'ENOENT'].includes(error.code)) throw error; }
  }
  fs.writeFileSync(require('node:path').join(workspace, 'probe-written'), 'ok');
  const connection = net.createConnection(socketPath);
  let connected = false;
  connection.setTimeout(1000, () => connection.destroy(Error('socket connection timed out')));
  connection.on('connect', () => { connected = true; connection.destroy(); console.error('AF_UNIX connection succeeded'); process.exitCode = 19; });
  connection.on('error', error => {
    if (connected) return;
    if (!['EPERM', 'EACCES'].includes(error.code)) { console.error('AF_UNIX failure not an enforced denial:', error.code); process.exitCode = 20; return; }
    process.stdout.write('PERI_SANDBOX_PROBE_OK\\n');
    setTimeout(() => {}, 3500);
  });
} catch (error) { console.error(error); process.exitCode = 21; }
`;

async function properties(unit: string): Promise<Map<string, string>> {
  const result = await command('systemctl', [
    '--user',
    'show',
    unit,
    '-p',
    'ActiveState',
    '-p',
    'PrivateTmp',
    '-p',
    'KillMode',
    '-p',
    'MemoryMax',
    '-p',
    'TasksMax',
    '-p',
    'CPUQuotaPerSecUSec',
    '-p',
    'ControlGroup',
    '-p',
    'BindsTo',
  ]);
  if (result.status) throw new ControlError('unsupported_environment', `cannot inspect probe unit: ${result.output}`);
  return new Map(
    result.output
      .trim()
      .split('\n')
      .map((line) => {
        const index = line.indexOf('=');
        return [line.slice(0, index), line.slice(index + 1)];
      }),
  );
}

export function verifyProbeCgroup(root: string, profile: Config['profiles'][string]): void {
  let events: string, cpu: string[], memory: string, tasks: string;
  try {
    events = readFileSync(join(root, 'cgroup.events'), 'utf8');
    cpu = readFileSync(join(root, 'cpu.max'), 'utf8').trim().split(/\s+/);
    memory = readFileSync(join(root, 'memory.max'), 'utf8').trim();
    tasks = readFileSync(join(root, 'pids.max'), 'utf8').trim();
  } catch {
    throw new ControlError('unsupported_environment', 'probe cgroup controller files unavailable');
  }
  const memoryMatch = /^(\d+)([KMGT])$/.exec(profile.memoryMax);
  const quotaMatch = /^(\d+)%$/.exec(profile.cpuQuota);
  if (
    !memoryMatch ||
    !quotaMatch ||
    !/^populated 1$/m.test(events) ||
    cpu.length !== 2 ||
    ![...cpu, memory, tasks].every((value) => /^\d+$/.test(value) && BigInt(value) > 0n)
  ) {
    throw new ControlError('unsupported_environment', 'probe cgroup controllers ineffective');
  }
  const bytes = BigInt(memoryMatch[1]) * 1024n ** BigInt('KMGT'.indexOf(memoryMatch[2]) + 1);
  if (
    BigInt(memory) !== bytes ||
    BigInt(tasks) !== BigInt(profile.tasksMax) ||
    BigInt(cpu[0]) * 100n !== BigInt(cpu[1]) * BigInt(quotaMatch[1])
  ) {
    throw new ControlError('unsupported_environment', 'probe cgroup limits differ from profile');
  }
}

function verifyProperties(
  values: Map<string, string>,
  config: Config,
  profile: Config['profiles'][string],
  unit: string,
  requireApiActive: boolean,
): void {
  const group = values.get('ControlGroup');
  if (
    values.get('ActiveState') !== 'active' ||
    values.get('PrivateTmp') !== 'yes' ||
    values.get('KillMode') !== 'control-group'
  )
    throw new ControlError('unsupported_environment', 'probe unit isolation properties not active');
  if (requireApiActive && !values.get('BindsTo')?.split(' ').includes(config.provider.apiUnit))
    throw new ControlError('unsupported_environment', 'probe unit not bound to API unit');
  if (!group?.startsWith('/user.slice/') || !group.endsWith(`/${unit}`) || group.includes('..'))
    throw new ControlError('unsupported_environment', 'probe unit has no verifiable cgroup');
  const root = resolve('/sys/fs/cgroup', '.' + group);
  if (!root.startsWith('/sys/fs/cgroup/user.slice/'))
    throw new ControlError('unsupported_environment', 'unexpected cgroup root');
  verifyProbeCgroup(root, profile);
  if (
    ['infinity', '[not set]', undefined].includes(values.get('MemoryMax')) ||
    ['infinity', '[not set]', undefined].includes(values.get('TasksMax')) ||
    ['infinity', '[not set]', undefined].includes(values.get('CPUQuotaPerSecUSec'))
  )
    throw new ControlError('unsupported_environment', 'resource limit property missing');
}

export function transientUnitProperties(
  config: Config,
  profile: Config['profiles'][string],
  bindApi: boolean,
): string[] {
  return [
    '-p',
    'Type=exec',
    '-p',
    'Restart=no',
    '-p',
    'KillMode=control-group',
    '-p',
    'OOMPolicy=continue',
    '-p',
    'PrivateTmp=yes',
    '-p',
    'ExecStartPre=/usr/bin/mkdir -p /tmp/claude',
    ...(bindApi ? ['-p', `BindsTo=${config.provider.apiUnit}`, '-p', `After=${config.provider.apiUnit}`] : []),
    '-p',
    `MemoryMax=${profile.memoryMax}`,
    '-p',
    `TasksMax=${profile.tasksMax}`,
    '-p',
    `CPUQuota=${profile.cpuQuota}`,
  ];
}

export function verifySeccompHelper(): void {
  const srtPackage = srtRuntime().packageRoot;
  if (process.arch !== 'arm64' && process.arch !== 'x64')
    throw new ControlError('unsupported_environment', `unsupported SRT architecture: ${process.arch}`);
  const helper = join(srtPackage, 'vendor', 'seccomp', process.arch === 'arm64' ? 'arm64' : 'x64', 'apply-seccomp');
  try {
    accessSync(helper, constants.X_OK);
  } catch {
    throw new ControlError('unsupported_environment', `SRT seccomp helper missing: ${helper}`);
  }
}

export async function probeEnvironment(
  config: Config,
  requireApiActive: boolean,
  stop: (unit: string) => Promise<void>,
): Promise<void> {
  if (process.platform !== 'linux' || process.getuid?.() === 0)
    throw new ControlError('unsupported_environment', 'requires an unprivileged Linux systemd user manager');
  if (!existsSync('/sys/fs/cgroup/cgroup.controllers'))
    throw new ControlError('unsupported_environment', 'cgroup v2 unavailable');
  const controllers = readFileSync('/sys/fs/cgroup/cgroup.controllers', 'utf8').split(/\s+/);
  if (!['cpu', 'memory', 'pids'].every((value) => controllers.includes(value)))
    throw new ControlError('unsupported_environment', 'required cgroup controllers unavailable');
  const manager = await command('systemctl', ['--user', 'is-system-running']);
  if (manager.status && !['running', 'degraded'].includes(manager.output.trim()))
    throw new ControlError('unsupported_environment', 'systemd user manager unavailable');
  if (requireApiActive) {
    const api = await command('systemctl', ['--user', 'show', config.provider.apiUnit, '-p', 'ActiveState', '--value']);
    if (api.status || api.output.trim() !== 'active')
      throw new ControlError('unsupported_environment', 'API user unit not active');
  }
  verifySeccompHelper();
  mkdirSync(config.controlRoot, { recursive: true, mode: 0o700 });
  mkdirSync(config.sandboxesRoot, { recursive: true, mode: 0o700 });
  if (!Object.keys(config.profiles).length) throw new ControlError('unsupported_environment', 'no profile to probe');
  for (const profileName of Object.keys(config.profiles))
    await probeProfile(config, requireApiActive, stop, profileName);
}

async function probeProfile(
  config: Config,
  requireApiActive: boolean,
  stop: (unit: string) => Promise<void>,
  profileName: string,
): Promise<void> {
  const id = `sbx_${randomBytes(16).toString('hex')}`;
  const generation = `run_${randomBytes(12).toString('hex')}`;
  const unit = `${config.provider.unitPrefix}${id}-${generation}.service`;
  const base = sandboxPath(config, id),
    workspace = join(base, 'workspace'),
    home = join(base, 'home');
  const policyPath = join(config.controlRoot, `probe-${id}.json`);
  const secretPath = join(config.controlRoot, `probe-secret-${id}`);
  const tmpMarker = join('/tmp', `peri-sandbox-host-${id}`);
  const socketPath = join(workspace, 'unix-probe.sock');
  const profile = config.profiles[profileName];
  const server = createServer((connection) => connection.end('unexpected access'));
  let child: ReturnType<typeof spawn> | undefined;
  let exit: Promise<number> | undefined;
  let exited = false;
  let stopped = false;
  try {
    makeDirectories(config, id);
    writeFileSync(policyPath, policyFor(config, id, profileName).json, { mode: 0o600 });
    writeFileSync(secretPath, 'control-secret', { mode: 0o600 });
    writeFileSync(tmpMarker, 'host-tmp-marker', { mode: 0o600 });
    await new Promise<void>((done, reject) => {
      const onError = (error: Error) => reject(error);
      server.once('error', onError).listen(socketPath, () => {
        server.off('error', onError);
        done();
      });
    });
    const args = [
      '--user',
      '--pipe',
      '--wait',
      '--quiet',
      `--unit=${unit}`,
      ...transientUnitProperties(config, profile, requireApiActive),
      '--',
      '/usr/bin/env',
      '-i',
      `PATH=${dirname(process.execPath)}:/usr/bin:/bin`,
      `HOME=${home}`,
      `XDG_CONFIG_HOME=${join(home, '.config')}`,
      'TMPDIR=/tmp',
      'CLAUDE_CODE_TMPDIR=/tmp/claude',
      'LANG=C.UTF-8',
      process.execPath,
      srtRuntime().cli,
      '--settings',
      policyPath,
      '--',
      '/usr/bin/setsid',
      '--fork',
      '--wait',
      process.execPath,
      '-e',
      runtimeProbe,
      socketPath,
      secretPath,
      tmpMarker,
      workspace,
    ];
    child = spawn('systemd-run', args, {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        PATH: '/usr/bin:/bin',
        XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR ?? '',
        DBUS_SESSION_BUS_ADDRESS: process.env.DBUS_SESSION_BUS_ADDRESS ?? '',
      },
    });
    let stdout = '',
      stderr = '';
    child.stdout!.on('data', (data: Buffer) => {
      if (stdout.length < 8192) stdout += data.toString().slice(0, 8192 - stdout.length);
    });
    child.stderr!.on('data', (data: Buffer) => {
      if (stderr.length < 8192) stderr += data.toString().slice(0, 8192 - stderr.length);
    });
    exit = new Promise<number>((done) => {
      child!.once('error', () => {
        exited = true;
        done(1);
      });
      child!.once('close', (code) => {
        exited = true;
        done(code ?? 1);
      });
    });
    const limit = Date.now() + Math.max(config.limits.startupMs, 12000);
    let observed = false;
    while (Date.now() < limit) {
      if (stdout.includes('PERI_SANDBOX_PROBE_OK')) {
        verifyProperties(await properties(unit), config, profile, unit, requireApiActive);
        observed = true;
        break;
      }
      if (exited) break;
      await new Promise((done) => setTimeout(done, 100));
    }
    if (!observed)
      throw new ControlError(
        'unsupported_environment',
        `SRT isolation probe failed: ${stderr.slice(0, 2000)} ${stdout.slice(0, 500)}`,
      );
    let timeout: NodeJS.Timeout | undefined;
    const status = await Promise.race([
      exit,
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(new ControlError('unsupported_environment', 'SRT probe did not exit')), 5000);
      }),
    ]).finally(() => clearTimeout(timeout));
    if (status || /seccomp.{0,120}(warning|failed|missing|unavailable)/i.test(stderr))
      throw new ControlError('unsupported_environment', `SRT probe exited or degraded: ${stderr.slice(0, 2000)}`);
  } finally {
    if (child && exit && !exited) child.kill('SIGKILL');
    try {
      if (exit) {
        let timeout: NodeJS.Timeout | undefined;
        await Promise.race([
          exit,
          new Promise<never>((_, reject) => {
            timeout = setTimeout(
              () => reject(new ControlError('stop_incomplete', 'probe launcher did not exit')),
              5000,
            );
          }),
        ]).finally(() => clearTimeout(timeout));
      }
    } finally {
      try {
        if (child) await stop(unit);
        stopped = true;
      } finally {
        if (server.listening) await new Promise<void>((done) => server.close(() => done()));
        if (stopped) {
          rmSync(base, { recursive: true, force: true });
          rmSync(policyPath, { force: true });
          rmSync(secretPath, { force: true });
          rmSync(tmpMarker, { force: true });
        }
      }
    }
  }
}
