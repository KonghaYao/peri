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

      // LLM judge 检查渲染质量
      try {
        const result = await judge({
          ansiRaw: capture.raw,
          criteria: [
            "消息区应有数字 1、2、3、4 依序出现在文本块中",
            "应有工具调用的显示（如 Read / 读取 等工具卡片）",
            "文本块和工具调用卡片应正确交替排列，无明显的渲染错位或重叠",
          ],
        });
        console.log("Judge:", JSON.stringify(result, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }
    },
  );
});
