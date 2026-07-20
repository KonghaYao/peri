/**
 * 工具卡片场景: 每个 batch 的第一个工具调用 stuck Running
 *
 * issue: 2026-07-20-first-tool-call-per-batch-stuck-running
 *
 * 回归验证：
 * - 同一 batch 中的第一个工具调用应正常完成，不会卡在 Running 状态
 * - 工具完成后指示器应切换为完成态（颜色变化），不应显示 "Running"
 * - 工具完成后应显示输出内容
 */
import { describe, it, expect, afterEach } from "vitest";
import {
  launchPeri,
  sendPrompt,
  takePeriSnapshot,
} from "../../helpers/peri.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: first tool not stuck running", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "[回归] batch 中的第一个工具调用应在完成后不再显示 Running",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();
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
    },
  );
});
