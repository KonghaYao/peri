/**
 * 本地开发默认存储：启动平台版 peri-loom 容器并导出连接信息。
 *
 * 只依赖 Node 内置模块与 docker CLI，供 smoke 脚本、SDK 集成测试与 CI 共用。
 * 排障通道：成对设置 `PERI_TEST_STORAGE_URL` / `PERI_TEST_STORAGE_TOKEN` 时直连外部
 * libSQL 兼容端点，否则自起平台版容器（每容器独立 run-dir，避免 launcher.lock 互斥）。
 */
import { execFile, spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { chmod, mkdtemp, realpath, rm } from 'node:fs/promises';
import { rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { connect, createServer } from 'node:net';
import { promisify } from 'node:util';

export const LOOM_IMAGE = 'ghcr.io/konghayao/peri-loom:v1.1.24';

const READY_PATTERN = /"ready"\s*:\s*true/;
const DOCKER_TIMEOUT_MS = 120_000;
const run = promisify(execFile);

/** 只透出 stderr：connection.json 的凭据不能进入错误消息或日志。 */
async function docker(args) {
  try {
    return await run('docker', args, { timeout: DOCKER_TIMEOUT_MS, maxBuffer: 8 * 1024 * 1024 });
  } catch (error) {
    const stderr = typeof error?.stderr === 'string' ? error.stderr.trim() : '';
    throw new Error(`docker ${args[0]} failed${stderr ? `: ${stderr}` : ''}`);
  }
}

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

function tail(text, lines = 40) {
  return text.split('\n').slice(-lines).join('\n');
}

export async function unusedPort() {
  const listener = createServer();
  listener.listen(0, '127.0.0.1');
  await once(listener, 'listening');
  const address = listener.address();
  if (!address || typeof address === 'string') throw new Error('no TCP port');
  await new Promise((done) => listener.close(done));
  return address.port;
}

/** 日志 ready 后监听可能还差几毫秒，端口探测在窗口内轮询而不是只试一次。 */
async function reachable(port, timeoutMs = 5_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const open = await new Promise((resolve) => {
      const socket = connect(port, '127.0.0.1');
      socket.setTimeout(500);
      socket.once('connect', () => { socket.destroy(); resolve(true); });
      socket.once('error', () => { socket.destroy(); resolve(false); });
      socket.once('timeout', () => { socket.destroy(); resolve(false); });
    });
    if (open) return true;
    await sleep(100);
  }
  return false;
}

/** 容器退出或超时都在错误里带上 docker logs 尾部，保留失败诊断上下文。 */
export async function waitReady(containerName, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const logs = await docker(['logs', containerName]).catch(() => ({ stdout: '', stderr: '' }));
    const output = `${logs.stdout}\n${logs.stderr}`;
    if (READY_PATTERN.test(output)) return;
    const state = await docker(['inspect', '-f', '{{.State.Running}}', containerName])
      .then((result) => result.stdout.trim())
      .catch(() => 'false');
    if (state !== 'true') throw new Error(`peri-loom exited before ready:\n${tail(output)}`);
    if (Date.now() >= deadline) {
      throw new Error(`peri-loom did not become ready within ${timeoutMs}ms:\n${tail(output)}`);
    }
    await sleep(200);
  }
}

export async function readLogs(containerName, { lines = 40 } = {}) {
  const result = await docker(['logs', '--tail', String(lines), containerName]).catch(() => null);
  if (!result) return '';
  return `${result.stdout}\n${result.stderr}`.trim();
}

export async function readConnection(containerName) {
  const { stdout } = await docker(['exec', containerName, 'cat', '/data/peri/connection.json']);
  let connection;
  try {
    connection = JSON.parse(stdout);
  } catch {
    throw new Error('peri-loom connection.json is not valid JSON');
  }
  const endpoint = String(connection.endpoint ?? '').replace(/\/+$/, '').replace('://0.0.0.0', '://127.0.0.1');
  const namespace = connection.database?.namespace ?? {};
  const tenant = namespace.tenant;
  const database = namespace.database;
  const branch = connection.database?.branch;
  const token = connection.project_api_key;
  if (!endpoint || !tenant || !database || !branch || !token) {
    throw new Error('peri-loom connection.json is missing endpoint, database or project_api_key');
  }
  return { endpoint, url: `${endpoint}/db/${tenant}/${database}/${branch}`, token, database: connection.database };
}

export async function stopLoom({ containerName, runDir } = {}) {
  if (containerName) await docker(['rm', '-f', containerName]).catch(() => {});
  if (runDir) await rm(runDir, { recursive: true, force: true }).catch(() => {});
}

/** 进程被信号打断时也要清掉容器与临时目录；exit 处理正常退出。 */
function registerCleanup(owned) {
  const cleanup = () => {
    try {
      spawnSync('docker', ['rm', '-f', owned.containerName], { stdio: 'ignore', timeout: 15_000 });
    } catch {}
    try { rmSync(owned.runDir, { recursive: true, force: true }); } catch {}
  };
  const onSignal = (code) => () => { cleanup(); process.exit(code); };
  const handlers = { exit: cleanup, SIGINT: onSignal(130), SIGTERM: onSignal(143) };
  process.once('exit', handlers.exit);
  process.once('SIGINT', handlers.SIGINT);
  process.once('SIGTERM', handlers.SIGTERM);
  return () => {
    process.off('exit', handlers.exit);
    process.off('SIGINT', handlers.SIGINT);
    process.off('SIGTERM', handlers.SIGTERM);
  };
}

async function startAttempt({ port, image, hostNetwork }) {
  const runDir = await realpath(await mkdtemp(join(tmpdir(), 'peri-loom-')));
  await chmod(runDir, 0o700);
  const containerName = `peri-loom-${randomUUID().slice(0, 8)}`;
  const args = ['run', '-d', '--name', containerName];
  if (hostNetwork) {
    args.push('--network', 'host');
  } else {
    args.push('-p', `127.0.0.1:${port}:${port}`);
  }
  args.push('-v', `${runDir}:/data/peri`, '--env', 'PERI_HRANA_EXPERIMENTAL=1', image,
    '--run-dir', '/data/peri', '--backend', 'local',
    '--listen', hostNetwork ? `127.0.0.1:${port}` : `0.0.0.0:${port}`, '--no-build');
  const owned = { containerName, runDir, port };
  try {
    await docker(args);
    await waitReady(containerName);
    if (!(await reachable(port))) throw new Error(`peri-loom is not reachable on 127.0.0.1:${port}`);
    return owned;
  } catch (error) {
    await stopLoom(owned);
    throw error;
  }
}

/**
 * 平台版启动即建库、签发凭据并把结果写进 run-dir 根目录的 connection.json。
 * host 网络在部分 Docker Desktop 上被禁用，探测失败后改用端口映射重起一次。
 */
export async function startLoom(options = {}) {
  const port = options.port ?? (await unusedPort());
  const image = options.image ?? LOOM_IMAGE;
  // host 网络在部分 Docker Desktop 上被禁用；回退端口映射时换新端口，
  // 避免上一次尝试的容器网络代理尚未释放导致绑定冲突。
  const owned = await startAttempt({ port, image, hostNetwork: true })
    .catch(async () => startAttempt({ port: await unusedPort(), image, hostNetwork: false }));
  const connection = await readConnection(owned.containerName).catch(async (error) => {
    await stopLoom(owned);
    throw error;
  });
  const detach = registerCleanup(owned);
  return {
    ...connection,
    ...owned,
    owned: true,
    logs: (logOptions) => readLogs(owned.containerName, logOptions),
    stop: async () => {
      detach();
      await stopLoom(owned);
    },
  };
}

/** 成对提供外部端点时直连；只给一半沿用 config.ts 的成对约定直接报错。 */
export async function lookupEnv(options = {}) {
  const url = process.env.PERI_TEST_STORAGE_URL;
  const token = process.env.PERI_TEST_STORAGE_TOKEN;
  if (url && token) return { url, token, owned: false, logs: async () => '', stop: async () => {} };
  if (url || token) {
    throw new Error('PERI_TEST_STORAGE_URL and PERI_TEST_STORAGE_TOKEN must be provided together');
  }
  return await startLoom(options);
}
