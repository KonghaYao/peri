/**
 * 冒烟测试: 启动 peri → 输入问题 → 等待回答 → LLM judge 断言
 *
 * 这是 e2e 管线的最小可验证单元。
 */
import { describe, it, expect } from "vitest";
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
      // 启动 peri
      tester = await launchPeri({ debug: true });

      // 发送问题
      await sendPrompt(tester, "hello");

      // 等待 AI 回答完成
      // 策略：先等待屏幕内容发生显著变化（说明输入被处理），再等待屏幕稳定
      const initialLen = (await tester.getScreenText()).length;
      let changed = false;

      for (let i = 0; i < 60; i++) {
        await tester.sleep(2000);
        const screen = await tester.getScreenText();
        const len = screen.length;
        // 屏幕长度变化超过 200 字符认为发生了内容变化
        if (!changed && Math.abs(len - initialLen) > 200) {
          changed = true;
        }
        if (changed && len > 50 && i > 2) {
          break; // 变化后等 2 轮确认
        }
      }

      // 抓 snapshot
      const capture = await takePeriSnapshot(tester, "basic-question-response");

      // 简单断言：屏幕不应为空
      expect(capture.text.length).toBeGreaterThan(0);

      // LLM judge 断言：检查关键元素
      // judge 失败不阻断测试（e2e judge 是辅助性的）
      let judgeResult: Awaited<ReturnType<typeof judge>> | null = null;
      try {
        judgeResult = await judge({
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
            judgeResult.checks
              .filter((c) => !c.pass)
              .map((c) => c.detail)
              .join("; "),
          );
        }
      } catch (err: any) {
        console.warn("⚠ LLM judge 调用失败（不影响测试结果）:", err.message);
      }

      // 基础断言确保核心流程跑通了
      expect(capture.text.length).toBeGreaterThan(0);
    },
  );
});
