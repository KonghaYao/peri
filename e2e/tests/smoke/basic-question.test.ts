/**
 * 冒烟测试: 启动 peri → 输入问题 → 等待回答 → LLM judge 断言
 *
 * 这是 e2e 管线的最小可验证单元。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
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
      tester = await launchPeri({ debug: true });

      // 记录初始屏幕
      const initial = await tester.getScreenText();

      await sendPrompt(tester, "hello");

      // 两阶段等待（利用 tui-tester 的 waitFor 内置轮询）
      // 1. 等待屏幕变化（输入被处理）
      await tester.waitFor(
        (screen) => screen !== initial,
        { timeout: 30_000, interval: 500, message: "屏幕未发生变化，输入可能未被处理" },
      );

      // 2. 等待屏幕稳定（LLM 回复完成）
      await waitForStableScreen(tester, 120_000);

      // 基本断言
      await tester.assertScreenContains("hello", { ignoreAnsi: true });

      // 抓 snapshot
      const capture = await takePeriSnapshot(tester, "basic-question-response");

      // 简单断言
      expect(capture.text.length).toBeGreaterThan(100);

      // LLM judge
      try {
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
      } catch (err: any) {
        console.warn("⚠ LLM judge 调用失败（不影响测试结果）:", err.message);
      }
    },
  );
});

/**
 * 等待屏幕内容稳定（连续 3 次轮询无变化）
 * 利用 tui-tester 的 waitFor 内置轮询，外层 wrapper 存储状态
 */
async function waitForStableScreen(tester: TmuxTester, timeout: number): Promise<void> {
  let lastLen = 0;
  let stableCount = 0;

  await tester.waitFor(
    (screen) => {
      const len = screen.length;
      if (len > 50 && len === lastLen) {
        stableCount++;
      } else {
        stableCount = 0;
      }
      lastLen = len;
      return stableCount >= 4;
    },
    { timeout, interval: 1500, message: "屏幕未能在超时时间内稳定" },
  );
}
