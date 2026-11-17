/**
 * 独立 Sandbox MCP 服务示例：本地 engine + 真实 peri-sandbox + actor `/mcp`。
 *
 * 运行前先在当前目录安装依赖并构建（见 README），再提供：
 *   SANDBOX_MCP_TOKEN=<actor secret>  PERI_PATH=<peri binary>
 * 可选：
 *   PERI_SANDBOX_SETTINGS / PERI_SANDBOX_WORKSPACE（已有授权目录）
 *   PERI_MACHINE_ID、PERI_ENGINE_PORT、RIVETKIT_STORAGE_PATH
 *
 * 只打印不含凭据的 URL；token 不进 URL 或日志。Chat 侧只消费该 URL。
 */
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { defaultProfile } from 'peri-sandbox';
import { createHostResourceOwner, WorkspaceURI } from '@peri-code/rivet/rivet';
import { createPeriSandboxMcpRuntime } from '@peri-code/rivet/sandbox';
import { createSandboxMcpActor } from '@peri-code/rivet/rivet';
import { setup } from 'rivetkit';
import { createClient } from 'rivetkit/client';

const token = process.env.SANDBOX_MCP_TOKEN;
if (!token) {
  console.error('缺少 SANDBOX_MCP_TOKEN；服务不会以无凭据方式启动。');
  process.exit(1);
}
const periPath = resolve(process.env.PERI_PATH ?? join(homedir(), '.peri', 'peri'));
if (!existsSync(periPath)) {
  console.error(`找不到 Peri 可执行文件：${periPath}；用 PERI_PATH 指定。`);
  process.exit(1);
}
// Rivet 状态固定到执行目录，不用全局 ~/.rivetkit。
process.env.RIVETKIT_STORAGE_PATH ??= resolve('.rivetkit');

const machineId = process.env.PERI_MACHINE_ID ?? '00000000-0000-4000-8000-000000000001';
const enginePort = Number(process.env.PERI_ENGINE_PORT ?? 8962);
const holder = 'sandbox-mcp:host';
const settingsPathEnv = process.env.PERI_SANDBOX_SETTINGS;
const workspaceEnv = process.env.PERI_SANDBOX_WORKSPACE;
if ((settingsPathEnv === undefined) !== (workspaceEnv === undefined)) {
  console.error('PERI_SANDBOX_SETTINGS 与 PERI_SANDBOX_WORKSPACE 必须同时提供或同时省略。');
  process.exit(1);
}
const temporary = settingsPathEnv === undefined;
const root = temporary ? realpathSync(mkdtempSync(join(tmpdir(), 'peri-sandbox-mcp-'))) : undefined;
if (temporary) {
  mkdirSync(join(root, 'workspace'), { mode: 0o700 });
}
const workspace = realpathSync(workspaceEnv ?? join(root, 'workspace'));
const settingsPath = settingsPathEnv ?? join(root, 'settings.json');
const sandboxesRoot = temporary ? join(root, 'sandboxes') : undefined;

if (temporary) {
  mkdirSync(sandboxesRoot, { mode: 0o700 });
  writeFileSync(
    settingsPath,
    JSON.stringify({
      sandboxesRoot,
      periPath,
      periSha256: createHash('sha256').update(readFileSync(periPath)).digest('hex'),
      defaultProfile: 'default',
      profiles: { default: defaultProfile(periPath) },
      startupTimeoutMs: 90_000,
      stopTimeoutMs: 20_000,
    }),
    { mode: 0o600 },
  );
}

const uri = new WorkspaceURI({ machineId, runtimePath: workspace }).toString();
const runtime = createPeriSandboxMcpRuntime({
  machineId,
  workspaces: [{ uri, path: workspace }],
  settingsPath,
  ...(sandboxesRoot === undefined ? {} : { sandboxesRoot }),
  startupTimeoutMs: 90_000,
  stopTimeoutMs: 20_000,
  minFreeBytes: 0,
});
// 宿主强持有：vars 丢失后旧 gate/实例关闭仍可枚举重试。
const owner = createHostResourceOwner();
const sandbox = createSandboxMcpActor({
  owner,
  runtime,
  token,
  machineId,
  authorizedWorkspaces: [uri],
  drainTimeoutMs: 5_000,
});

const registry = setup({
  use: { sandbox },
  startEngine: true,
  engineHost: '127.0.0.1',
  enginePort,
  startServices: false,
  namespace: 'default',
  noWelcome: true,
  shutdown: { disableSignalHandlers: true },
  logging: { level: 'error' },
});
const client = createClient({
  endpoint: `http://127.0.0.1:${enginePort}`,
  namespace: 'default',
  disableMetadataLookup: true,
  headers: { 'x-peri-sandbox-token': token },
});

let held = false;
try {
  await registry.startAndWait();
  const sbx = client.sandbox.getOrCreate(['sbx', uri]);
  const acquired = await sbx.acquire(holder, token);
  if (!acquired.ok) throw new Error(acquired.error.code);
  held = true;

  // 只含地址、actor id 与 path；凭据由 Chat 以 header 发送。
  const url = new URL(`/gateway/${encodeURIComponent(await sbx.resolve())}/request/mcp`, `http://127.0.0.1:${enginePort}`);
  console.log(`SANDBOX_MCP_URL=${url.href}`);
  console.log(`SANDBOX_MCP_WORKSPACE=${uri}`);
  console.log('本地 spawn engine 的 gateway token 为 default；SANDBOX_MCP_TOKEN 仅作为 actor/HTTP 凭据。');
  console.log('先停 Chat 使用者，再对本进程发送 SIGINT/SIGTERM。');

  await new Promise((resolve) => {
    process.once('SIGINT', resolve);
    process.once('SIGTERM', resolve);
  });
} catch (error) {
  console.error(`sandbox mcp service failed: ${error instanceof Error ? error.name : typeof error}`);
  process.exitCode = 1;
} finally {
  try {
    if (held) {
      const released = await client.sandbox.getOrCreate(['sbx', uri]).release(holder, token);
      if (!released.ok) {
        console.error(`release incomplete: ${released.error.code}`);
        process.exitCode = 1;
      }
    }
  } catch (error) {
    console.error(`release failed: ${error instanceof Error ? error.name : typeof error}`);
    process.exitCode = 1;
  }
  // 关闭 actor 准入后，host owner 只报告已确认的 gate/instance；未确认时保留重试权。
  owner.beginShutdown();
  let hostConfirmed = false;
  try {
    const report = await owner.closeAll({ timeoutMs: 5_000 });
    hostConfirmed = report.complete;
    if (!report.complete) {
      console.error(`host resources incomplete: ${report.pending}`);
      process.exitCode = 1;
    }
  } catch (error) {
    console.error(`host close failed: ${error instanceof Error ? error.name : typeof error}`);
    process.exitCode = 1;
  }
  // 兜底 runtime 回收只在 owner 确认所有 gate/instance 已关闭后执行；未确认的 stream 仍可能在用
  // 实例与临时 state 目录，不能用进程级 closeAll/rmSync 假装清理完成。
  let runtimeConfirmed = false;
  if (hostConfirmed) {
    try {
      await runtime.closeAll();
      runtimeConfirmed = true;
    } catch (error) {
      console.error(`runtime closeAll failed: ${error instanceof Error ? error.name : typeof error}`);
      process.exitCode = 1;
    }
  } else {
    console.error('runtime closeAll deferred: host resources are not confirmed');
  }
  await client.dispose().catch(() => undefined);
  await registry.shutdown().catch(() => undefined);
  if (runtimeConfirmed && temporary && root !== undefined) rmSync(root, { recursive: true, force: true });
}

// 清理由 release/closeAll/shutdown 显式完成；原生 runtime 可能保留事件句柄，
// 这里结束 CLI 事件循环，让退出码反映清理结果。
await new Promise((resolve) => setTimeout(resolve, 0));
process.exit(process.exitCode ?? 0);
