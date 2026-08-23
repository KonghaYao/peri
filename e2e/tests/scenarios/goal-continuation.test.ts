/**
 * 场景测试 3: Goal 续跑中断唤醒
 *
 * 验证 Goal 机制在 agent 中断后能自动唤醒继续执行。
 * agent 在每个 turn 末尾检查未完成 goal，注入续跑信号。
 *
 * prompt 来源: prompts/ai-text-in-streaming.md
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("scenarios: goal continuation", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "Goal 任务在中断后自动续跑直到完成",
    { timeout: 600_000 },
    async () => {
      tester = await launchPeri();

      // 发送 goal 指令：agent 需要数到 10
      // goal 机制会在每个 turn 末尾注入续跑信号，直到 goal 完成
      const baseScreen = await tester.getScreenText();
      await sendPrompt(
        tester,
        "我们来测试 goal 工具， 你要数到 10 ，但是中间中断， 让 goal 唤醒你",
      );

      // Goal 可能跨多个 turn 自动续跑；以提交前屏幕为基线，等待最终完整屏幕稳定。
      await waitForStableScreen(tester, 180_000, baseScreen);

      const capture = await takePeriSnapshot(tester, "goal-continuation-complete");

      expect(capture.text.length).toBeGreaterThan(100);

      // LLM judge
      const result = await judge({
        ansiRaw: capture.raw,
        criteria: [
          "消息区应最终包含数字 10（或表明计数到达了 10）",
          "agent 应完成了 counting 任务，而非中途放弃",
        ],
      });
      console.log("Judge:", JSON.stringify(result, null, 2));
      expect(result.pass).toBe(true);
    },
  );
});
