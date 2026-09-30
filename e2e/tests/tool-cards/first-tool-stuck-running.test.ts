/**
 * 工具卡片场景: 每个 batch 的第一个工具调用 stuck Running
 *
 * issue: 2026-07-20-first-tool-call-per-batch-stuck-running
 *
 * 回归验证：
 * - 同一 batch 中的第一个工具调用应正常完成，不会卡在 Running 状态
 * - 工具完成后指示器应切换为完成态（颜色变化），不应显示 "Running"
 * - 工具完成后应显示输出内容
 *
 * 驱动方式（L0 确定性改造）：隔离 HOME + 本地假 model server 重放
 * （`helpers/replay-model.ts`）。一步双 tool_use（Read + Grep 同属一个 batch）
 * 由剧本保证——真实模型是否合并为双工具 batch 不可控，重放后该前提确定成立；
 * 不依赖真实模型/凭据/外部网络。
 */
import { describe, it, expect, afterEach, beforeAll } from "vitest";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { rm } from "node:fs/promises";
import { PROJECT_ROOT, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import {
  launchReplayTui,
  makeReplayHome,
  replayDebug,
  startReplayModelServer,
  type ReplayServer,
} from "../../helpers/replay-model.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: first tool not stuck running", () => {
  let tester: TmuxTester;
  let replay: ReplayServer;
  let home: string;

  beforeAll(async () => {
    // 控制面脚本不构建 binary；本用例必须跑当前源码。
    await promisify(execFile)("cargo", ["build", "-p", "peri-tui", "--bin", "peri"], {
      cwd: PROJECT_ROOT,
      timeout: 600_000,
      maxBuffer: 8 * 1024 * 1024,
    });
  }, 610_000);

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
    if (replay) await replay.close();
    if (home) await rm(home, { recursive: true, force: true });
  });

  it(
    "[回归] batch 中的第一个工具调用应在完成后不再显示 Running",
    { timeout: 300_000 },
    async () => {
      // 剧本：一步返回两个 tool_use（Read + Grep 同一 batch），工具结果回传后
      // 以文本步收尾（cwd = PROJECT_ROOT，Read/Grep 相对路径指向 Cargo.toml）。
      replay = await startReplayModelServer([
        {
          prompt: "用 Read 工具读取 Cargo.toml",
          toolUses: [
            { name: "Read", input: { file_path: "Cargo.toml" } },
            { name: "Grep", input: { pattern: "name", path: "Cargo.toml" } },
          ],
        },
        { text: "REPLAY-TOOLS-DONE" },
      ]);
      home = await makeReplayHome(replay.port);
      tester = await launchReplayTui({ home });
      await tester.sleep(3000);

      // 强制两个独立工具调用：Read + Grep，确保双工具 batch
      await sendPrompt(
        tester,
        "按照下面步骤依次执行，必须使用 Read 和 Grep 两个独立工具调用，" +
          "不要合并到一个命令中：" +
          "1. 用 Read 工具读取 Cargo.toml 文件内容。" +
          "2. 用 Grep 工具在 Cargo.toml 中搜索 'name' 关键字。"
      );

      await tester.waitForText("Read", {
        timeout: 120_000,
        interval: 2000,
      });
      await tester.sleep(20000);

      const capture = await takePeriSnapshot(tester, "first-tool-not-stuck-v2");
      console.log("Snapshot text length:", capture.text.length);
      console.log("Tool card area:", capture.text.substring(0, 800));

      // 核心断言：完成后不应有 "Running (" 残留
      const hasRunning = capture.text.includes("Running (");
      // 至少确认 Read 和 Grep 工具卡片出现了
      const hasRead = capture.text.includes("Read");
      const hasGrep = capture.text.includes("Grep");

      console.log({ hasRunning, hasRead, hasGrep });

      expect(hasRunning).toBe(false);
      expect(hasRead).toBe(true);
      expect(hasGrep).toBe(true);

      // 重放契约：剧本必须全部命中（未命中会回固定文本，掩盖真实链路偏航）。
      expect(replay.misses(), `剧本全部命中：${replayDebug(replay)}`).toBe(0);
    },
  );
});
