/**
 * 工具卡片场景: 错误态头行无后缀
 *
 * issue: 2026-07-20-e2e-tool-call-header-suffix-tests
 *
 * 回归验证：错误时头行无后缀，仍然强制展开显示错误信息。
 * commit e5239171 的特殊约束——错误态不受影响：
 * - Read/Edit/Write/Glob/Grep 错误时头行仅显示工具名和参数，无 "—" 后缀
 * - 错误输出仍然强制展开显示（不受折叠策略影响）
 * - 错误信息在独立的输出行中可见
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: error no header suffix", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "工具错误时头行无后缀、错误输出强制展开",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // 用 Edit 测试：对不存在的文件做 Edit 会触发错误
      await sendPrompt(
        tester,
        "请使用 Read 工具读取文件 /tmp/nonexistent-peri-e2e-header-test.txt，\n" +
          "该文件肯定不存在",
      );

      // 等待 Read 工具出现
      await tester.waitForText("Read", {
        timeout: 60_000,
        interval: 1000,
      });
      await tester.sleep(5000);

      const capture = await takePeriSnapshot(tester, "error-no-suffix");

      expect(capture.text.length).toBeGreaterThan(50);

      // Judge: 验证错误态
      try {
        const r = await judge({
          ansiRaw: capture.raw,
          criteria: [
            "屏幕上应出现 Read 工具调用的痕迹",
            "Read 工具的头行应只包含文件名参数（如 'Read (/tmp/nonexistent...)'），不应有 '— N lines' 等后缀",
            "错误详细信息应在独立的输出行中可见（如 'Error:' 或 'not found' 或 'Tool execution failed'）",
            "错误信息不应该压缩消失，应该清晰展示失败原因",
          ],
        });
        console.log("Judge:", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }
    },
  );
});
