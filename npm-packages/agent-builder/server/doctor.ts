import { statfsSync } from 'node:fs';
import { probeChatRuntime, wasmAssetsAvailable } from './chats/engine';
import { configFailure } from './config-error';
import { loadEnv, type ServerEnv } from './env';
import { diagnosticCode } from './errors';

export type DoctorLine = {
  readonly status: 'ok' | 'warn' | 'blocked';
  readonly label: string;
  readonly detail: string;
};

export interface DoctorReport {
  readonly ready: boolean;
  readonly blocked: boolean;
  readonly lines: readonly DoctorLine[];
}

/** Store URLs may embed userinfo or query credentials; only the origin and path are shown. */
function safeUrl(raw: string): string {
  try {
    const url = new URL(raw);
    return `${url.protocol}//${url.host}${url.pathname}`;
  } catch {
    return '<invalid URL>';
  }
}

function freeMiB(directory: string): number | null {
  try {
    const stats = statfsSync(directory);
    return Math.floor((Number(stats.bavail) * Number(stats.bsize)) / (1024 * 1024));
  } catch {
    return null;
  }
}

/** 离线基础设施报告；不联网、不读取 Provider DB，也不输出凭据或 query。 */
export function buildDoctorReport(env: ServerEnv): DoctorReport {
  const lines: DoctorLine[] = [];
  const runtime = probeChatRuntime(env);

  lines.push({
    status: 'ok',
    label: '机器身份',
    detail: `${env.machineId}（持久化，跨重启稳定）`,
  });
  lines.push({
    status: 'ok',
    label: '工作区 URI',
    detail: env.workspaceUri,
  });
  lines.push({
    status: 'ok',
    label: '工作区目录',
    detail: env.workspacePath,
  });
  lines.push({
    status: env.storeToken === undefined ? 'blocked' : 'ok',
    label: 'Peri Store',
    detail: `${safeUrl(env.storeUrl)}${env.storeToken === undefined ? '（缺少 PERI_STORE_TOKEN）' : '（凭据已配置）'}`,
  });

  lines.push({
    status: 'ok',
    label: '模型 Provider',
    detail:
      env.provider.type === undefined
        ? '未设置 PERI_PROVIDER_TYPE；启动时不会同步 Provider，doctor 不读取凭据，未执行模型认证'
        : `由 PERI_PROVIDER_* 环境变量同步为 ${env.provider.type}；doctor 不读取凭据，未执行模型认证`,
  });
  lines.push({
    status: wasmAssetsAvailable() ? 'ok' : 'blocked',
    label: 'WASM 运行时',
    detail: wasmAssetsAvailable() ? 'SDK wasm 资源存在' : 'SDK 安装中缺少 dist/wasm',
  });
  lines.push({
    status: env.rivetStoragePath === undefined ? 'blocked' : 'ok',
    label: 'Rivet 数据目录',
    detail: env.rivetStoragePath ?? '缺少 RIVETKIT_STORAGE_PATH（请用 agent-builder start 启动）',
  });

  const sandbox = env.sandbox;
  lines.push({
    status: sandbox.mode === 'ready' ? 'ok' : sandbox.enabled ? 'blocked' : 'warn',
    label: 'Sandbox',
    detail:
      sandbox.mode === 'ready'
        ? `已启用（${sandbox.periPath ?? ''}）`
        : sandbox.enabled
          ? `已启用但配置不完整：${sandbox.reasons.join(', ')}`
          : '未启用（设置 PERI_SANDBOX_ENABLED=true 并配置 PERI_SANDBOX_PERI_PATH）',
  });
  if (sandbox.enabled) {
    lines.push({
      status: sandbox.workspacePathPrivate ? 'ok' : 'blocked',
      label: 'Sandbox 工作区权限',
      detail: sandbox.workspacePathPrivate ? '0700' : '工作区必须为 0700 且不能是 symlink',
    });
  }

  const disk = freeMiB(env.workspacePath);
  lines.push({
    status: disk === null ? 'warn' : disk < 512 ? 'warn' : 'ok',
    label: '可用磁盘',
    detail: disk === null ? '无法读取' : `${disk} MiB${disk < 512 ? '（Sandbox 可能无法启动）' : ''}`,
  });

  const blocked = runtime.mode !== 'wasm' || (sandbox.enabled && sandbox.mode !== 'ready');
  return { ready: !blocked, blocked, lines };
}

export function renderDoctor(report: DoctorReport): string {
  const prefix = { ok: 'OK   ', warn: 'WARN ', blocked: 'BLOCK' } as const;
  return [
    ...report.lines.map((line) => `${prefix[line.status]} ${line.label}: ${line.detail}`),
    '',
    report.ready
      ? '结论：基础设施配置完整，可以启动；Provider 由 PERI_PROVIDER_* 同步，未执行模型认证。'
      : '结论：尚未就绪；按上面的 BLOCK 项修正后重试。',
  ].join('\n');
}

/** Entry point for `agent-builder doctor`; returns the process exit code. */
export function runDoctor(source: Record<string, string | undefined> = process.env): number {
  let env: ServerEnv;
  try {
    env = loadEnv(source);
  } catch (error) {
    const failure = configFailure(error);
    if (failure !== null) {
      console.error(`BLOCK 环境配置: ${failure.hint}`, { code: failure.code });
    } else {
      console.error('BLOCK 环境配置: 配置无效，请检查本地配置', { code: diagnosticCode(error) });
    }
    return 1;
  }
  const report = buildDoctorReport(env);
  console.log(renderDoctor(report));
  if (report.ready) return 0;
  // Store/WASM 等基础设施阻塞与每 Provider 的凭据资格分开。
  return 2;
}
