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

/**
 * reasoning 是否处于展开态。Feature/20260811 重构后 Completed reasoning
 * 首行统一 `│` 竖线（与 Running 同形，视觉连续），不再用 ▸/▾ 箭头区分
 * （§6.4，render.rs「折叠/展开差异由正文是否渲染承担」）——展开态只能靠
 * 「摘要行下一行是否紧邻 reasoning 正文」判定：
 * - 展开：摘要行（`│ Thought for 12s · N lines`）下一行直接是 reasoning
 *   正文（以 `│` 开头，无空行分隔，§6.3 紧凑贴合）；
 * - 折叠：摘要行下一行为空行（md 正文前导空行）或下一 entry 符号行
 *   （工具卡 `✓`/`⠇`，非 `│` 开头）——均不误判。
 * 摘要为硬编码英文（对齐工具卡片口径，§6.4），匹配 `Thought`（含无时长
 * 变体 `Thought · N lines`）；保留 zh 关键词匹配以兼容旧版本输出。
 */
function hasExpandedReasoning(text: string): boolean {
  const lines = text.split("\n");
  return lines.some(
    (l, i) =>
      (l.includes("思考了") || l.includes("Thought")) &&
      i + 1 < lines.length &&
      lines[i + 1].trimStart().startsWith("│"),
  );
}

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

      // Turn 完成后 reasoning 应按 §7 自动折叠为单行（摘要行后无正文行）
      const afterTurn = await tester.getScreenText();
      expect(hasExpandedReasoning(afterTurn)).toBe(false);

      // 手动展开：Alt+Up 逐条上移 entry 焦点（显式 CSI 序列 \e[1;3A，裁决 C3），
      // Enter 切换 Collapsed/Expanded。reasoning 可能不在末位 entry
      // （工具交错时在中间 assistant bubble），循环直至展开（摘要行后出现正文）。
      let expanded = false;
      for (let i = 0; i < 12; i++) {
        await tester.sendText("\u001b[1;3A"); // Alt+Up
        await tester.sleep(150);
        await tester.sendKey("enter");
        await tester.sleep(350);
        if (hasExpandedReasoning(await tester.getScreenText())) {
          expanded = true;
          break;
        }
      }
      expect(expanded).toBe(true);

      // 第二轮 streaming：手动展开的 reasoning 不得被自动折叠
      const base2 = await tester.getScreenText();
      // [Fix] Alt+Up 循环后 entry 焦点仍激活（FOCUSED_ENTRY 非空）——此时
      // Enter 被消息区仲裁为折叠切换（focus_router::message_nav_accepts，
      // 「entry 导航模式」设计语义），sendPrompt 的提交 Enter 会被抢走、
      // prompt 不提交；waitForStableScreen 会把「折叠变化 + 稳定」误判为
      // turn 完成，滚顶后第一轮 reasoning 已被折叠、断言必失败。
      // 先 Esc 退出导航（清 FOCUSED_ENTRY）再提交。
      await tester.sendKey("escape");
      await tester.sleep(150);
      await sendPrompt(tester, "再简单回答一句话即可。");
      await waitForStableScreen(tester, 180_000, base2);

      // 滚回顶部检查第一轮的 reasoning 仍展开（摘要行后正文可见）
      await tester.sendKey("home", { ctrl: true });
      await tester.sleep(400);
      const final = await tester.getScreenText();
      expect(hasExpandedReasoning(final)).toBe(true);
    },
  );
});
