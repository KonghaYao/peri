/**
 * 冒烟测试: 启动 peri → 输入问题 → 等待回答 → LLM judge 断言
 *
 * 这是 e2e 管线的最小可验证单元。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import { updateJudgeResult } from "../../helpers/recorder.js";
import type { TmuxTester } from "tui-tester";

describe("peri e2e smoke", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "启动后提问并验证回答",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // 记录提交前的屏幕作为基准（用于 waitForStableScreen 先等变化）
      const initial = await tester.getScreenText();

      await sendPrompt(tester, "hello");

      // 两阶段等待：先等变化，再等稳定
      await waitForStableScreen(tester, 120_000, initial);

      // 基本断言
      await tester.assertScreenContains("hello", { ignoreAnsi: true });

      // 抓 snapshot
      const capture = await takePeriSnapshot(tester, "basic-question-response");

      // 简单断言
      expect(capture.text.length).toBeGreaterThan(100);

      // LLM judge
      const judgeResult = await judge({
        ansiRaw: capture.raw,
        criteria: [
          "消息区域应该有 AI 的回复内容（不只是空白或加载状态）",
          "界面底部应该有输入框区域",
        ],
      });

      await updateJudgeResult(
        "启动后提问并验证回答",
        1,
        judgeResult.pass ? "pass" : "fail",
        judgeResult.checks,
        judgeResult.duration_ms,
      );

      console.log("Judge 结果:", JSON.stringify(judgeResult, null, 2));

      if (!judgeResult.pass) {
        console.warn(
          "⚠ LLM judge 判定不通过:",
          judgeResult.checks.filter((c) => !c.pass).map((c) => c.detail).join("; "),
        );
      }
      expect(judgeResult.pass).toBe(true);
    },
  );
});
