/**
 * 场景测试 1: 流式文本与工具调用交错输出
 *
 * 验证 AI 在流式输出文本的同时穿插工具调用（如 Read），
 * 消息区能正确渲染文本块和工具卡片，不会出现渲染错位。
 *
 * prompt 来源: prompts/ai-text-in-streaming.md
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("scenarios: streaming + tool interleave", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "流式文本与工具调用交错输出不渲染错位",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // 记录提交前的屏幕作为基准
      const base = await tester.getScreenText();

      // 要求 agent: 说 1 → 两次 Read → 说 2 → 两次 Read → 到 4
      await sendPrompt(
        tester,
        "请你说一句 1 然后调用两次 read 工具读取 README.md 找附近的文件，然后说 2，两次 read 读取 package.json 中的项目路径，重复直到 4。注意每次 read 都要读不同的文件。",
      );

      await waitForStableScreen(tester, 180_000, base);

      const capture = await takePeriSnapshot(tester, "streaming-tool-interleave");

      // 基本断言
      expect(capture.text.length).toBeGreaterThan(200);

      // LLM judge 检查渲染质量（不测试 agent 行为）
      const result = await judge({
        ansiRaw: capture.raw,
        criteria: [
          "屏幕中应至少有 2 个 Read 工具调用卡片（绿色圆点标记）",
          "思考文本块（如'思考了 N 字符'）和工具调用卡片应可见且排列有序，无文本重叠",
          "不应出现严重的渲染错位（如文字覆盖、行重叠、截断）",
          "状态栏应显示上下文消耗（如格式 'NN% NNNk'），且百分比数值合理（>0% 且 <=100%）",
        ],
      });
      console.log("Judge:", JSON.stringify(result, null, 2));
      expect(result.pass).toBe(true);
    },
  );

  it(
    "手动展开 reasoning 后继续 streaming 不被自动折叠",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();
      const base = await tester.getScreenText();

      // 第一轮：产生 reasoning + 简短回答
      await sendPrompt(
        tester,
        "请先思考（reasoning）再回答：读取 README.md 的第一行，然后用一句话回答你读到了什么。",
      );
      await waitForStableScreen(tester, 180_000, base);

      // Turn 完成后 reasoning 应按 §7 自动折叠为单行（无 ⏿ tail 前缀行）
      const afterTurn = await tester.getScreenText();
      expect(afterTurn).not.toContain("\u{23bf}");

      // 手动展开：Alt+Up 逐条上移 entry 焦点（显式 CSI 序列 \e[1;3A，裁决 C3），
      // Enter 切换 Collapsed/Expanded。reasoning 可能不在末位 entry
      // （工具交错时在中间 assistant bubble），循环直至 ⏿ 出现。
      let expanded = false;
      for (let i = 0; i < 12; i++) {
        await tester.sendText("\u001b[1;3A"); // Alt+Up
        await tester.sleep(150);
        await tester.sendKey("enter");
        await tester.sleep(350);
        if ((await tester.getScreenText()).includes("\u{23bf}")) {
          expanded = true;
          break;
        }
      }
      expect(expanded).toBe(true);

      // 第二轮 streaming：手动展开的 reasoning 不得被自动折叠
      const base2 = await tester.getScreenText();
      await sendPrompt(tester, "再简单回答一句话即可。");
      await waitForStableScreen(tester, 180_000, base2);

      // 滚回顶部检查第一轮的 reasoning 仍展开（body tail 的 ⏿ 前缀可见）
      await tester.sendKey("home", { ctrl: true });
      await tester.sleep(400);
      const final = await tester.getScreenText();
      expect(final).toContain("\u{23bf}");
    },
  );
});
