/**
 * 场景测试 2: AskUserQuestion 面板交互
 *
 * 验证 agent 调用 AskUserQuestion 工具时，
 * TUI 显示内联问答面板，用户可通过键盘操作选择答案。
 *
 * prompt 来源: prompts/ai-text-in-streaming.md
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

async function sendInputAndWait(
  tester: TmuxTester,
  key: "space" | "Tab" | "Enter",
  predicate: (screen: string) => boolean,
  label: string,
): Promise<void> {
  await tester.sendKey(key);
  await tester.waitFor(predicate, {
    timeout: 10_000,
    interval: 100,
    message: label,
  });
}

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

      await sendPrompt(
        tester,
        '这是 E2E 测试。请立即调用 AskUserQuestion 工具，不得只解释。参数必须包含三个问题，每题四个选项：第一题 id=language、header=编程语言、question=你最喜欢的编程语言是哪个？、单选 Rust/TypeScript/Python/Go；第二题 id=review、header=代码审查、question=代码审查时你关注哪些方面？、多选 正确性/安全性/性能/可维护性；第三题 id=terminal、header=终端偏好、question=你偏好哪种终端？、单选 iTerm2/Terminal/WezTerm/Alacritty。收到回答后请总结三个回答。',
      );

      // 等待面板出现（内联面板标题为 "Ask User"）
      // 注意：不能用 "AskUserQuestion"——用户 prompt 里就含这个文本，会过早匹配
      await tester.waitForText("Ask User", {
        timeout: 120_000,
        interval: 500,
      });

      // 立即抓面板 snapshot
      const panelCapture = await takePeriSnapshot(tester, "ask-user-question-panel");

      // 与面板交互：Space 选中，Tab 显式切到下一题，最后 Enter 提交。
      // 每次选择都等待公开选择标记出现；切题后等待标记消失，确保 handler
      // 已消费按键，而不是按固定 sleep 猜测组件是否完成挂载/重渲染。
      await tester.sleep(1000);
      for (let q = 0; q < 3; q++) {
        await sendInputAndWait(
          tester,
          "space",
          (screen) => /(?:^|\n)\s*[●☑]\s/m.test(screen),
          `等待第 ${q + 1} 题选中标记`,
        );
        if (q < 2) {
          await sendInputAndWait(
            tester,
            "Tab",
            (screen) => !/(?:^|\n)\s*[●☑]\s/m.test(screen),
            `等待切换到第 ${q + 2} 题`,
          );
        } else {
          await sendInputAndWait(
            tester,
            "Enter",
            (screen) => !screen.includes("Ask User"),
            "等待提交 AskUserQuestion",
          );
        }
      }

      // 分两阶段等待：面板关闭证明 Submit 已被消费；随后等主 turn footer。
      // 最终回复较长时工具行可能滚出视口，不能依赖它与 footer 同屏。
      await tester.waitFor(
        (screen) => !screen.includes("Ask User"),
        {
          timeout: 120_000,
          interval: 500,
          message: "等待 AskUserQuestion 面板关闭并完成",
        },
      );
      await tester.waitFor(
        (screen) => /(?:Brewed for|处理耗时)/.test(screen),
        {
          timeout: 120_000,
          interval: 500,
          message: "等待 AskUserQuestion 主 turn 完成",
        },
      );

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
