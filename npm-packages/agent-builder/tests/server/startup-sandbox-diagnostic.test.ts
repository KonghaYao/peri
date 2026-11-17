import { afterEach, describe, expect, test } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { chmodSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { connect, createServer } from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { configFailure } from '../../server/config-error';
import { diagnosticCode } from '../../server/errors';

// 真实入口：与打包 CLI 相同，启动失败必须发生在监听之前。
const ENTRY = fileURLToPath(new URL('../../server/main.ts', import.meta.url));
const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function makeRoot(base: string): string {
  const root = realpathSync(mkdtempSync(path.join(base, 'agent-builder-startup-')));
  chmodSync(root, 0o700);
  roots.push(root);
  return root;
}

function makeWorkspace(parent: string): string {
  const dir = path.join(parent, 'workspace');
  mkdirSync(dir, { mode: 0o700 });
  chmodSync(dir, 0o700);
  return dir;
}

interface CliResult {
  readonly status: number | null;
  readonly stdout: string;
  readonly stderr: string;
}

function baseEnv(root: string, workspacePath: string): Record<string, string> {
  return {
    HOME: path.join(root, 'home'),
    PATH: process.env.PATH ?? '/usr/bin:/bin',
    TMPDIR: root,
    DATABASE_PATH: path.join(root, 'data', 'agent-builder.db'),
    PERI_WORKSPACE_ID: 'startup-diagnostic',
    PERI_WORKSPACE_PATH: workspacePath,
    PERI_STORE_URL: 'http://127.0.0.1:58240/db/local_default/agent_builder/main',
    PERI_STORE_TOKEN: '',
    ANTHROPIC_API_KEY: '',
    OPENAI_API_KEY: '',
    PERI_SANDBOX_ENABLED: 'false',
    PERI_SANDBOX_PERI_PATH: path.join(root, 'no-peri'),
  };
}

function runEntry(args: readonly string[], env: Record<string, string>, cwd: string): CliResult {
  const result = spawnSync(process.execPath, [ENTRY, ...args], {
    cwd,
    env,
    encoding: 'utf8',
    // 正确实现会在监听前失败；超时用于防止回归后挂起。
    timeout: 15_000,
  });
  return { status: result.status, stdout: result.stdout ?? '', stderr: result.stderr ?? '' };
}

function combined(result: CliResult): string {
  return `${result.stdout}\n${result.stderr}`;
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      server.close(() => {
        if (address === null || typeof address === 'string') reject(new Error('no TCP address'));
        else resolve(address.port);
      });
    });
  });
}

function isPortOpen(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = connect({ host: '127.0.0.1', port });
    let settled = false;
    const finish = (open: boolean) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      resolve(open);
    };
    socket.setTimeout(500);
    socket.once('connect', () => finish(true));
    socket.once('error', () => finish(false));
    socket.once('timeout', () => finish(false));
  });
}

async function waitForPort(port: number, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await isPortOpen(port)) return true;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return false;
}

describe('startup sandbox deployment diagnostics', () => {
  test('sandbox enabled without peri reports an actionable code and never listens', async () => {
    const secret = `sk-p02-${randomBytes(16).toString('hex')}`;
    const root = makeRoot(realpathSync(os.tmpdir()));
    const workspace = makeWorkspace(root);
    const missingPeri = path.join(root, `peri-${secret}`);
    const port = await freePort();
    const env = {
      ...baseEnv(root, workspace),
      PERI_SANDBOX_ENABLED: 'true',
      PERI_SANDBOX_PERI_PATH: missingPeri,
      PORT: String(port),
      PERI_ENGINE_PORT: String(await freePort()),
    };

    const doctor = runEntry(['--doctor'], env, root);
    const doctorOutput = combined(doctor);
    expect(doctor.status).toBe(2);
    expect(doctorOutput).toContain('sandbox_peri_missing');
    expect(doctorOutput).not.toContain('internal_error');
    expect(doctorOutput).not.toContain(secret);

    const start = runEntry([], env, root);
    const output = combined(start);
    expect(start.status).toBe(1);
    expect(output).toContain('sandbox_peri_missing');
    expect(output).toContain('PERI_SANDBOX_PERI_PATH');
    expect(output).not.toContain('internal_error');
    expect(output).not.toContain(secret);
    expect(output).not.toContain(missingPeri);
    expect(output).not.toContain('api listening');
    expect(await isPortOpen(port)).toBe(false);
  });

  test('unexpected startup failures keep internal_error without echoing error fields', async () => {
    const secret = `sk-p02-${randomBytes(16).toString('hex')}`;
    const root = makeRoot(realpathSync(os.tmpdir()));
    const workspace = makeWorkspace(root);
    const blocker = path.join(root, `blocker-${secret}`);
    writeFileSync(blocker, '');
    const port = await freePort();

    const result = runEntry(
      [],
      {
        ...baseEnv(root, workspace),
        DATABASE_PATH: path.join(blocker, 'agent-builder.db'),
        PORT: String(port),
        PERI_ENGINE_PORT: String(await freePort()),
      },
      root,
    );
    const output = combined(result);

    expect(result.status).toBe(1);
    expect(output).toContain('internal_error');
    expect(output).not.toContain(secret);
    expect(output).not.toContain(blocker);
    expect(output).not.toContain('api listening');
    expect(await isPortOpen(port)).toBe(false);
  });

  test('unknown error message, name, stack and cause never become a trusted code', () => {
    const secret = `sk-p02-${randomBytes(16).toString('hex')}`;
    const error = new Error(secret);
    error.name = secret;
    error.stack = secret;
    (error as Error & { cause?: unknown }).cause = new Error(secret);

    expect(configFailure(error)).toBeNull();
    expect(diagnosticCode(error)).toBe('internal_error');
    // 只有结构化的 ConfigError 能成为可信 code；带 code 字段的任意对象仍走兜底。
    expect(configFailure({ code: 'sandbox_peri_missing', message: secret })).toBeNull();
    expect(diagnosticCode({ code: secret })).toBe('internal_error');
  });

  test('sandbox-disabled startup still listens and exits cleanly on SIGTERM', async () => {
    const root = makeRoot(realpathSync(os.tmpdir()));
    const workspace = makeWorkspace(root);
    const port = await freePort();
    const proc = Bun.spawn([process.execPath, ENTRY], {
      cwd: root,
      env: {
        ...baseEnv(root, workspace),
        PORT: String(port),
        PERI_ENGINE_PORT: String(await freePort()),
      },
      stdout: 'pipe',
      stderr: 'pipe',
    });
    const stdoutText = new Response(proc.stdout).text();
    const stderrText = new Response(proc.stderr).text();

    try {
      const listening = await waitForPort(port, 10_000);
      if (!listening) {
        proc.kill('SIGTERM');
        await proc.exited;
        throw new Error(`startup did not listen\n${await stdoutText}\n${await stderrText}`);
      }
      expect(await isPortOpen(port)).toBe(true);
      proc.kill('SIGTERM');
      expect(await proc.exited).toBe(0);
      const output = `${await stdoutText}\n${await stderrText}`;
      expect(output).toContain('api listening');
      expect(output).not.toContain('sandbox_peri_missing');
      expect(output).not.toContain('internal_error');
    } finally {
      proc.kill('SIGTERM');
      await proc.exited;
    }
    expect(await isPortOpen(port)).toBe(false);
  });
});
