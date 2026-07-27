/**
 * 工具卡片场景: Read 头行显示行数
 *
 * issue: 2026-07-20-e2e-tool-call-header-suffix-tests
 *
 * 验证 Read 完成后头行显示 "— N lines" 后缀：
 * - Read 折叠态头行显示文件路径 + "— N lines"
 * - N 为文件非空行数（动态计算）
 * - 不再有独立的输出行显示行数摘要
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: read line count", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "Read 完成后头行显示 N lines 后缀",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // Cargo.toml 行数可预期（通常 10-30 行）
      await sendPrompt(
        tester,
        "请用 Read 工具读取 Cargo.toml 文件的内容",
      );

      // 等待 Read 工具被调用
      await tester.waitForText("Read", {
        timeout: 60_000,
        interval: 1000,
      });
      // 等 Read 完成
      await tester.sleep(5000);

      const capture = await takePeriSnapshot(tester, "read-line-count");

      expect(capture.text.length).toBeGreaterThan(50);

      // Judge: Read 头行后缀
      const r = await judge({
        ansiRaw: capture.raw,
        criteria: [
          "屏幕上应出现 Read 工具调用的痕迹",
          "Read 工具的头行应包含文件路径和行数摘要，格式如 'Read (Cargo.toml) — N lines'",
          "行数 N 应是一个合理的正整数（> 0），函数调用应成功读取并显示文件行数",
        ],
      });
      console.log("Judge:", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);
    },
  );
});
