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

// ---- tmux 环境检查与清理 ----

async function killTestSessions(): Promise<void> {
  try {
    const { stdout } = await execAsync(
      "tmux list-sessions -F '#{session_name}' 2>/dev/null || echo ''",
    );
    const sessions = stdout
      .split("\n")
      .filter((s) => s.includes("peri-e2e") || s.includes("tui-test") || s.includes("test-") || s.includes("minimal-"))
      .filter(Boolean);
    for (const session of sessions) {
      await execAsync(`tmux kill-session -t ${session} 2>/dev/null`).catch(
        () => {},
      );
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

  await killTestSessions();
  console.log("✓ e2e 测试环境就绪");
}, 30_000);

beforeEach(({ task }) => {
  if (task?.name) {
    setCurrentTestName(task.name);
  }
});

afterEach(async () => {
  await killTestSessions();
}, 10_000);

afterAll(async () => {
  await killTestSessions();
  resetCounters();
}, 30_000);

export {};
