import { describe, expect, test } from "bun:test";
import { resolve } from "node:path";

/** 进程级信号测试：spawn 真实入口，锁住关闭清理与退出码语义。 */

const PACKAGE_ROOT = resolve(import.meta.dir, "..");
const ENTRY = resolve(import.meta.dir, "index.ts");
const TSCONFIG = resolve(import.meta.dir, "tsconfig.json");
const MACHINE_ID = "11111111-1111-4111-8111-111111111111";
const LISTENING_MARKER = "[peri-cf] server listening";
const STARTUP_TIMEOUT_MS = 10_000;
const EXIT_TIMEOUT_MS = 15_000;
const TEST_TIMEOUT_MS = 30_000;

interface CapturedOutput {
  stdout: string;
  stderr: string;
}

type SpawnedProcess = ReturnType<typeof Bun.spawn>;

/** 探测一个当前空闲的端口；探测 server 立即停止，随后由子进程占用。 */
function freePort(): number {
  const probe = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch: () => new Response(null),
  });
  const port = probe.port;
  probe.stop(true);
  if (port === undefined) throw new Error("failed to reserve a TCP port");
  return port;
}

/** 只给子进程最小合法配置；不继承父测试进程的环境，避免真实凭据或遥测混入。 */
function serverEnvironment(port: number): Record<string, string> {
  return {
    PERI_MODEL_PROVIDER: "openai",
    PERI_MODEL_ID: "gpt-test",
    PERI_MODEL_BASE_URL: "https://api.example.com/v1",
    PERI_MODEL_API_KEY: "test-model-key",
    PERI_STORAGE_URL: "https://store.example.com",
    PERI_STORAGE_TOKEN: "test-store-token",
    PERI_MACHINE_ID: MACHINE_ID,
    PERI_SERVER_HOST: "127.0.0.1",
    PERI_SERVER_PORT: String(port),
    // WASM 只在首个 Action 时懒启动；本测试不发 Action。
    PERI_WASM_MODULE_URL: "file:///nonexistent/peri-wasm.js",
  };
}

async function capture(
  stream: ReadableStream<Uint8Array>,
  output: CapturedOutput,
  key: keyof CapturedOutput,
): Promise<void> {
  const decoder = new TextDecoder();
  const reader = stream.getReader();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (value) output[key] += decoder.decode(value, { stream: true });
    }
    output[key] += decoder.decode();
  } finally {
    reader.releaseLock();
  }
}

/** 等到入口打印监听日志；进程提前退出或超时都带上两路输出便于定位。 */
async function waitForListening(
  child: SpawnedProcess,
  output: CapturedOutput,
  outputDrained: Promise<void>,
): Promise<void> {
  const deadline = Date.now() + STARTUP_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (output.stdout.includes(LISTENING_MARKER)) return;
    if (child.exitCode !== null) {
      await outputDrained;
      throw new Error(
        `server exited during startup (code ${child.exitCode})`
        + `\nstdout:\n${output.stdout}\nstderr:\n${output.stderr}`,
      );
    }
    await Bun.sleep(20);
  }
  throw new Error(
    `server did not report listening within ${STARTUP_TIMEOUT_MS}ms`
    + `\nstdout:\n${output.stdout}\nstderr:\n${output.stderr}`,
  );
}

async function exitWithin(child: SpawnedProcess, timeoutMs: number): Promise<number> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      child.exited,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`process did not exit within ${timeoutMs}ms`)),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

describe("server process signals", () => {
  for (const signal of ["SIGTERM", "SIGINT"] as const) {
    test(`真实入口收到 ${signal} 后完成关闭并以 0 退出`, async () => {
      const output: CapturedOutput = { stdout: "", stderr: "" };
      const child = Bun.spawn(
        [process.execPath, "--tsconfig-override", TSCONFIG, ENTRY],
        {
          cwd: PACKAGE_ROOT,
          env: serverEnvironment(freePort()),
          stdout: "pipe",
          stderr: "pipe",
        },
      );
      const outputDrained = Promise.all([
        capture(child.stdout, output, "stdout"),
        capture(child.stderr, output, "stderr"),
      ]).then(() => {});
      try {
        await waitForListening(child, output, outputDrained);
        child.kill(signal);
        expect(await exitWithin(child, EXIT_TIMEOUT_MS)).toBe(0);
        await outputDrained;

        expect(output.stdout).toContain(LISTENING_MARKER);
        expect(output.stderr).not.toContain("shutdown is incomplete");
      } finally {
        if (child.exitCode === null) {
          child.kill("SIGKILL");
          await child.exited;
        }
        await outputDrained.catch(() => {});
      }
    }, TEST_TIMEOUT_MS);
  }
});
