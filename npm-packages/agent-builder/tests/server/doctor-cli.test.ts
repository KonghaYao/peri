import { afterEach, describe, expect, test } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// 真实入口：bin/agent-builder.js 只是在该模块前准备目录并追加 --doctor。
const ENTRY = fileURLToPath(new URL('../../server/main.ts', import.meta.url));
const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function makeRoot(base: string): string {
  const root = mkdtempSync(path.join(base, 'agent-builder-doctor-cli-'));
  chmodSync(root, 0o700);
  roots.push(root);
  return root;
}

function makeWorkspace(parent: string, name = 'workspace'): string {
  const dir = path.join(parent, name);
  mkdirSync(dir, { mode: 0o700 });
  chmodSync(dir, 0o700);
  return dir;
}

function tmpIsAlias(): boolean {
  try {
    return realpathSync('/tmp') !== '/tmp';
  } catch {
    return false;
  }
}

interface AliasFixture {
  readonly root: string;
  /** 用户按常见写法填入的路径。 */
  readonly configured: string;
  /** realpath 后的真实路径。 */
  readonly canonical: string;
  /** true 表示复用了 macOS 的 /tmp symlink，false 表示自建受控 symlink 父目录。 */
  readonly viaRealTmp: boolean;
}

/** 制造“父目录是 symlink、最终目录不是”的别名；不依赖平台是否自带 /tmp symlink。 */
function makeAliasWorkspace(secret: string): AliasFixture {
  if (tmpIsAlias()) {
    const root = makeRoot('/tmp');
    const configured = makeWorkspace(root, `workspace-${secret}`);
    return { root, configured, canonical: realpathSync(configured), viaRealTmp: true };
  }

  const root = makeRoot(realpathSync(os.tmpdir()));
  const realParent = makeWorkspace(root, 'real-parent');
  const workspace = makeWorkspace(realParent, `workspace-${secret}`);
  const aliasParent = path.join(root, 'alias-parent');
  symlinkSync(realParent, aliasParent);
  return {
    root,
    configured: path.join(aliasParent, `workspace-${secret}`),
    canonical: workspace,
    viaRealTmp: false,
  };
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
    PERI_WORKSPACE_ID: 'doctor-cli',
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
    // 正确实现不会启动服务；超时用于防止“别名被接受后挂起”掩盖回归。
    timeout: 15_000,
  });
  return { status: result.status, stdout: result.stdout ?? '', stderr: result.stderr ?? '' };
}

function combined(result: CliResult): string {
  return `${result.stdout}\n${result.stderr}`;
}

describe('doctor/startup workspace path diagnostics', () => {
  test('doctor ignores Provider env sentinels and only checks infrastructure without opening Provider DB or engine', () => {
    const root = makeRoot(realpathSync(os.tmpdir()));
    const workspace = makeWorkspace(root);
    const source = {
      ...baseEnv(root, workspace),
      PERI_STORE_TOKEN: randomBytes(24).toString('hex'),
      ANTHROPIC_API_KEY: randomBytes(24).toString('hex'),
      OPENAI_API_KEY: randomBytes(24).toString('hex'),
      RIVETKIT_STORAGE_PATH: path.join(root, 'rivet'),
    };
    const result = runEntry(['--doctor'], source, root);
    expect(result.status).toBe(0);
    const output = combined(result);
    expect(output).toContain('基础设施配置完整');
    expect(output).toContain('未执行模型认证');
    expect(output.includes(source.PERI_STORE_TOKEN)).toBe(false);
    expect(output.includes(source.ANTHROPIC_API_KEY) || output.includes(source.OPENAI_API_KEY)).toBe(false);
    expect(output).not.toContain('ANTHROPIC_API_KEY');
    expect(output).not.toContain('OPENAI_API_KEY');
    expect(output).toContain('PERI_PROVIDER_TYPE');
    expect(output).not.toContain('api listening');
    expect(existsSync(path.join(root, 'data', 'agent-builder.db'))).toBe(false);
  });
  test('replays the /tmp alias with a stable actionable code instead of internal_error', () => {
    const secret = `sk-diag-${randomBytes(16).toString('hex')}`;
    const fixture = makeAliasWorkspace(secret);
    // 夹具自检：同一目录的两种写法，真实路径与配置路径不同。
    expect(fixture.configured).not.toBe(fixture.canonical);
    expect(realpathSync(fixture.configured)).toBe(fixture.canonical);
    expect(fixture.viaRealTmp).toBe(tmpIsAlias());

    const result = runEntry(
      ['--doctor'],
      {
        ...baseEnv(fixture.root, fixture.configured),
        PERI_STORE_TOKEN: secret,
        ANTHROPIC_API_KEY: secret,
      },
      fixture.root,
    );
    const output = combined(result);

    expect(result.status).toBe(1);
    expect(output).toContain('workspace_path_not_canonical');
    expect(output).toContain('realpath');
    expect(output).not.toContain('internal_error');
    // 不泄露被拒绝的路径（路径里含合成 secret）、真实路径或 secret 本身。
    expect(output).not.toContain(secret);
    expect(output).not.toContain(fixture.configured);
    expect(output).not.toContain(fixture.canonical);
  });

  test('reports missing prerequisites for a real canonical workspace path', () => {
    const root = makeRoot(realpathSync(os.tmpdir()));
    const workspace = realpathSync(makeWorkspace(root));
    const result = runEntry(['--doctor'], baseEnv(root, workspace), root);
    const output = combined(result);

    expect(result.status).toBe(2);
    expect(output).toContain('缺少 PERI_STORE_TOKEN');
    expect(output).toContain('未执行模型认证');
    expect(output).toContain(`工作区目录: ${workspace}`);
    expect(output).not.toContain('internal_error');
    expect(output).not.toContain('workspace_path_');
  });

  test('still rejects a symlink workspace instead of normalizing it', () => {
    const secret = `sk-diag-${randomBytes(16).toString('hex')}`;
    const root = makeRoot(realpathSync(os.tmpdir()));
    const real = makeWorkspace(root, 'real-workspace');
    const link = path.join(root, `workspace-link-${secret}`);
    symlinkSync(real, link);

    const result = runEntry(
      ['--doctor'],
      {
        ...baseEnv(root, link),
        PERI_STORE_TOKEN: secret,
      },
      root,
    );
    const output = combined(result);

    expect(result.status).toBe(1);
    expect(output).toContain('workspace_path_symlink');
    expect(output).toContain('符号链接');
    expect(output).not.toContain('internal_error');
    expect(output).not.toContain(secret);
    expect(output).not.toContain(real);
  });

  test('keeps unrelated unexpected errors on the safe internal_error fallback', () => {
    const secret = `sk-diag-${randomBytes(16).toString('hex')}`;
    const root = makeRoot(realpathSync(os.tmpdir()));
    const workspace = makeWorkspace(root);
    const blocker = path.join(root, `blocker-${secret}`);
    writeFileSync(blocker, '');

    const result = runEntry(
      ['--doctor'],
      {
        ...baseEnv(root, workspace),
        DATABASE_PATH: path.join(blocker, 'agent-builder.db'),
      },
      root,
    );
    const output = combined(result);

    expect(result.status).toBe(1);
    expect(output).toContain('internal_error');
    expect(output).not.toContain('workspace_path_');
    // 原始 Error message 含 blocker 路径，不得出现在公共输出。
    expect(output).not.toContain(secret);
    expect(output).not.toContain(blocker);
  });

  test('rejects a symlink database ancestor with the safe internal_error boundary', () => {
    const secret = `sk-db-${randomBytes(16).toString('hex')}`;
    const root = makeRoot(realpathSync(os.tmpdir()));
    const workspace = makeWorkspace(root);
    const real = makeWorkspace(root, 'real-data');
    const privateDir = makeWorkspace(real, 'private');
    const alias = path.join(root, 'alias-data');
    symlinkSync(real, alias);
    const databasePath = path.join(alias, 'private', `config-${secret}.db`);

    const result = runEntry(
      ['--doctor'],
      {
        ...baseEnv(root, workspace),
        DATABASE_PATH: databasePath,
        PERI_STORE_TOKEN: secret,
        ANTHROPIC_API_KEY: secret,
      },
      root,
    );
    const output = combined(result);

    expect(result.status).toBe(1);
    expect(output).toContain('internal_error');
    expect(output).not.toContain(secret);
    expect(output).not.toContain(alias);
    expect(output).not.toContain(databasePath);
    expect(existsSync(path.join(privateDir, `config-${secret}.db`))).toBe(false);
    expect(existsSync(path.join(privateDir, 'machine-id'))).toBe(false);
  });

  test('startup prints the same actionable code instead of only internal_error', () => {
    const secret = `sk-diag-${randomBytes(16).toString('hex')}`;
    const fixture = makeAliasWorkspace(secret);
    const result = runEntry(
      [],
      {
        ...baseEnv(fixture.root, fixture.configured),
        PERI_STORE_TOKEN: secret,
        ANTHROPIC_API_KEY: secret,
        PORT: '58411',
        PERI_ENGINE_PORT: '58412',
      },
      fixture.root,
    );
    const output = combined(result);

    expect(result.status).toBe(1);
    expect(output).toContain('workspace_path_not_canonical');
    expect(output).toContain('realpath');
    expect(output).not.toContain('internal_error');
    expect(output).not.toContain(secret);
    expect(output).not.toContain(fixture.configured);
    expect(output).not.toContain(fixture.canonical);
  });
});
