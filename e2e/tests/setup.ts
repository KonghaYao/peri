/**
 * e2e 测试全局 setup
 */
import dotenv from "dotenv";

dotenv.config({ override: true }); // 加载 e2e/.env，覆盖全局环境变量
import { exec } from "node:child_process";
import { promisify } from "node:util";
import { beforeAll, afterAll, beforeEach, afterEach } from "vitest";
import { setCurrentTestName, resetCounters } from "../helpers/recorder.js";

const execAsync = promisify(exec);

// ---- 并行模式 ----

/**
 * 并行模式（由 scripts/run-e2e.mjs 设置 E2E_PARALLEL=1）：
 * 每个 vitest 进程是一个 worker，运行互不相交的测试文件子集。
 * 此时必须跳过"清理所有 tui-test-* session"的全局逻辑——
 * 否则一个 worker 的 afterEach/beforeAll 会杀掉其他 worker 正在使用的 tmux session。
 * 残留清理由控制面脚本在启动所有 worker 前统一执行一次。
 */
const isParallel = process.env.E2E_PARALLEL === "1";

// ---- tmux 环境检查与清理 ----

// 只清理 tui-tester 创建的残留 session（前缀 tui-test-），
// 避免误伤用户其他 tmux session；tmux server 不存在时静默通过。
// exec 显式带 timeout：tmux 命令异常挂起时不能卡住测试进程。
async function killTestSessions(): Promise<void> {
  try {
    const { stdout } = await execAsync(
      "tmux list-sessions -F '#{session_name}' 2>/dev/null || echo ''",
      { timeout: 15_000 },
    );
    const sessions = stdout
      .split("\n")
      .map((s) => s.trim())
      .filter((s) => s.startsWith("tui-test-"));
    for (const session of sessions) {
      await execAsync(`tmux kill-session -t ${session} 2>/dev/null`, {
        timeout: 15_000,
      }).catch(() => {});
    }
  } catch {
    // no sessions
  }
}

beforeAll(async () => {
  // 确保 tmux 在 PATH
  process.env.PATH = `/opt/homebrew/bin:/usr/local/bin:/usr/bin:${
    process.env.PATH || ""
  }`;

  try {
    await execAsync("which tmux");
  } catch {
    console.warn("⚠ tmux 未安装，e2e 测试将被跳过");
    return;
  }

  if (!isParallel) {
    await killTestSessions();
  }
  console.log("✓ e2e 测试环境就绪");
}, 30_000);

beforeEach(({ task }) => {
  if (task?.name) {
    setCurrentTestName(task.name);
  }
});

afterEach(async () => {
  // 串行模式：清理本测试可能的异常残留 session。
  // 并行模式：每个测试自身 afterEach 已 stop 自己的 tester；
  // 全局清理会误杀其他 worker 的 session，由控制面脚本统一清理。
  if (!isParallel) {
    await killTestSessions();
  }
}, 10_000);

afterAll(async () => {
  if (!isParallel) {
    await killTestSessions();
  }
  resetCounters();
}, 30_000);

export {};
