/**
 * 场景测试 2: AskUserQuestion 面板交互
 *
 * 验证 agent 调用 AskUserQuestion 工具时，
 * TUI 显示内联问答面板，用户可通过键盘操作选择答案。
 *
 * prompt 来源: prompts/ai-text-in-streaming.md
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("scenarios: ask user question", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "AskUserQuestion 面板出现并可交互",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // 记录提交前的屏幕（用于后续 waitForStableScreen 基准）
      const base = await tester.getScreenText();

      await sendPrompt(
        tester,
        "请你测试一下 AskUserQuestion 工具， 三个题目， 每个题目 4 个选项， 第一题单选， 第二题多选",
      );

      // 等待面板出现（内联面板标题为 "Ask User"）
      // 注意：不能用 "AskUserQuestion"——用户 prompt 里就含这个文本，会过早匹配
      await tester.waitForText("Ask User", {
        timeout: 60_000,
        interval: 500,
      });

      // 立即抓面板 snapshot
      const panelCapture = await takePeriSnapshot(tester, "ask-user-question-panel");

      // 与面板交互：3 道题，每道 Space 选中 → Enter 下一题（最后一题 Enter 提交）
      for (let q = 0; q < 3; q++) {
        await tester.sendKey("space");
        await tester.sleep(150);
        await tester.sendKey("Enter");
        await tester.sleep(300);
      }

      // 等待面板关闭、agent 恢复并完成（需先等屏幕变化再等稳定）
      await waitForStableScreen(tester, 120_000, base);

      const capture = await takePeriSnapshot(tester, "ask-user-question-complete");

      // 基本断言
      expect(panelCapture.text).toContain("Ask User");
      expect(capture.text.length).toBeGreaterThan(100);

      // LLM judge: 面板阶段
      const panelResult = await judge({
        ansiRaw: panelCapture.raw,
        criteria: [
          "屏幕上应有 Ask User 内联面板，包含题目文本和选项列表",
          "面板中应有可选选项（如 ●/○ 单选标记或 ☑/☐ 多选标记）",
        ],
      });
      console.log("Judge (panel):", JSON.stringify(panelResult, null, 2));
      expect(panelResult.pass).toBe(true);

      // LLM judge: 交互完成阶段——agent 收到答案后继续执行，输出总结
      const doneResult = await judge({
        ansiRaw: capture.raw,
        criteria: [
          "agent 应已完成了对 AskUserQuestion 工具的测试，输出了总结（如包含表格或结构化的测试结果）",
          "agent 的总结应体现 AskUserQuestion 交互已收到用户回答（如提及'三个题目均已正常返回'、'已收到回答'等表述或对回答内容的总结），而不是报错或中断",
        ],
      });
      console.log("Judge (done):", JSON.stringify(doneResult, null, 2));
      expect(doneResult.pass).toBe(true);
    },
  );
});
