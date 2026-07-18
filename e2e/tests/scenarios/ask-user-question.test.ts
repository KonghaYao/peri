/**
 * 场景测试 2: AskUserQuestion 弹窗交互
 *
 * 验证 agent 调用 AskUserQuestion 工具时，
 * TUI 弹出 HITL 弹窗，用户可通过键盘操作选择答案。
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
    "AskUserQuestion 弹窗出现并可交互",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      await sendPrompt(
        tester,
        "请你测试一下 AskUserQuestion 工具， 三个题目， 每个题目 4 个选项， 第一题单选， 第二题多选",
      );

      // 等待弹窗出现（HITL 弹窗会占据屏幕区域）
      await tester.waitForText("AskUserQuestion", {
        timeout: 30_000,
        interval: 500,
      });

      // 立即抓弹窗 snapshot（弹窗可能自动关闭）
      const popupCapture = await takePeriSnapshot(tester, "ask-user-question-popup");

      // 尝试与弹窗交互：选择第一个选项 → 确认
      // 弹窗交互: Tab 切换选项，Enter 确认
      await tester.sendKey("Tab");
      await tester.sleep(200);
      await tester.sendKey("Enter");
      await tester.sleep(300);

      // 等待弹窗关闭、agent 恢复并完成
      await waitForStableScreen(tester, 120_000);

      const capture = await takePeriSnapshot(tester, "ask-user-question-complete");

      // 基本断言
      expect(popupCapture.text).toContain("AskUserQuestion");
      expect(capture.text.length).toBeGreaterThan(100);

      // LLM judge
      try {
        const result = await judge({
          ansiRaw: popupCapture.raw,
          criteria: [
            "屏幕上应有 AskUserQuestion 相关的弹窗 UI 元素",
            "弹窗中应显示题目文本和选项列表",
          ],
        });
        console.log("Judge (popup):", JSON.stringify(result, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }
    },
  );
});
