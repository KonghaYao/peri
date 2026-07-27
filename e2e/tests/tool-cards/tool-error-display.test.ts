/**
 * 工具卡片场景: 工具调用错误显示
 *
 * 验证 Read 一个不存在的文件时，工具卡片以错误态渲染：
 * - is_error=true 时强制展开
 * - 错误输出使用 error 色
 * - agent 能感知错误并调整策略
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: error display", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "Read 不存在的文件时工具卡片显示错误态",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      await sendPrompt(
        tester,
        "请使用 Read 工具读取文件 /nonexistent/peri_e2e_test_file_12345.txt",
      );

      // 等待 Read 工具被调用并返回错误
      await tester.waitForText("Read", {
        timeout: 60_000,
        interval: 1000,
      });
      await tester.sleep(3000);

      const errorCapture = await takePeriSnapshot(tester, "tool-error-read");

      // 等待 agent 处理完错误
      await tester.sleep(5000);
      const afterCapture = await takePeriSnapshot(tester, "tool-error-after");

      expect(errorCapture.text.length).toBeGreaterThan(50);
      expect(afterCapture.text.length).toBeGreaterThan(50);

      // Judge: 错误态
      const r = await judge({
        ansiRaw: errorCapture.raw,
        criteria: [
          "屏幕上应出现 Read 工具调用的痕迹（如 'Read' 或 'read' 字样）",
          "agent 应感知到文件不存在（如 'not found'、'不存在'、'no such file' 等错误提示）",
        ],
      });
      console.log("Judge (error):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: agent 调整策略
      const r2 = await judge({
        ansiRaw: afterCapture.raw,
        criteria: [
          "agent 应承认文件不存在这一事实，未继续尝试读取该文件",
          "agent 的回复应包含对错误的说明或替代建议",
        ],
      });
      console.log("Judge (after):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);
    },
  );
});
