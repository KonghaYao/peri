/**
 * 场景测试 3: Goal 续跑中断唤醒
 *
 * 验证 Goal 机制在 agent 中断后能自动唤醒继续执行。
 * agent 在每个 turn 末尾检查未完成 goal，注入续跑信号。
 *
 * prompt 来源: prompts/ai-text-in-streaming.md
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
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

      // 使用短且可观测的两阶段协议：第一阶段保持 goal active，让 middleware
      // 自动注入续跑；第二阶段用 block 终止 fixture，避免把本用例耦合到独立的
      // auxiliary verifier JSON 兼容性。这里验证的是 continuation，不是 verifier。
      await sendPrompt(
        tester,
        "这是 Goal continuation E2E。必须先调用 goal(create)，objective 为分两阶段完成计数。" +
        "当前第一阶段只输出 GOAL_STAGE_1= 后接一至五的阿拉伯数字（逗号分隔），然后结束当前回复，保持 goal active，绝对不要继续第二阶段。" +
        "被 goal system reminder 自动唤醒后，第二阶段只输出 GOAL_STAGE_2= 后接六至十的阿拉伯数字（逗号分隔），" +
        "再调用 goal(block)，reason=e2e continuation verified。最后把 GOAL_CONTINUATION、_E2E_、DONE 三段连起来单独输出。",
      );

      // 最终 marker 未以完整字符串出现在 prompt 中，因此不会被输入回显提前命中。
      await tester.waitFor(
        (screen) =>
          screen.includes("GOAL_CONTINUATION_E2E_DONE") &&
          /(?:Brewed for|处理耗时)/.test(screen),
        {
          timeout: 300_000,
          interval: 1000,
          message: "等待 Goal 自动续跑后的终态 marker 超时",
        },
      );

      const capture = await takePeriSnapshot(tester, "goal-continuation-complete");

      expect(capture.text.length).toBeGreaterThan(100);
      expect(capture.text).toMatch(/GOAL_STAGE_2=\s*6,7,8,9,10/);
      expect(capture.text).toContain("GOAL_CONTINUATION_E2E_DONE");
      expect(capture.text).toMatch(/System Reminder|system-reminder/);
      expect(capture.text).toMatch(/ExecuteExtraTool action: block|Goal marked as blocked/);
    },
  );
});
