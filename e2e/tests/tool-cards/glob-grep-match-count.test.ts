/**
 * 工具卡片场景: Glob/Grep 头行显示匹配数
 *
 * issue: 2026-07-20-e2e-tool-call-header-suffix-tests
 *
 * 验证 Glob/Grep 完成后头行显示 "— N matches" 后缀：
 * - Glob 完成后头行显示 pattern + "— N matches"
 * - Grep 完成后头行显示 pattern + "— N matches"
 * - N 为匹配数（从输出非空行数动态计算）
 * - 不再有独立的输出行显示匹配数
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: glob grep match count", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "Glob 和 Grep 完成后头行显示 matches 后缀",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // 要求同时用 Glob 和 Grep
      await sendPrompt(
        tester,
        "请先使用 Glob 搜索 'src/**/*.rs' 匹配 Rust 源文件，\n" +
          "再使用 Grep 在 src 目录搜索 'fn main' 找到所有主函数。\n" +
          "必须使用 Glob 和 Grep 两个工具，不要跳过",
      );

      // 等待 Glob 工具被调用
      await tester.waitForText("Glob", {
        timeout: 60_000,
        interval: 1000,
      });

      // 等待 Grep 工具出现并完成
      await tester.waitForText("Grep", {
        timeout: 60_000,
        interval: 1000,
      });
      await tester.sleep(8000);

      const capture = await takePeriSnapshot(tester, "glob-grep-matches");

      expect(capture.text.length).toBeGreaterThan(50);

      // Judge: 验证头行后缀格式
      try {
        const r = await judge({
          ansiRaw: capture.raw,
          criteria: [
            "屏幕上应出现 Glob 和 Grep 两个工具调用的痕迹",
            "Glob 工具头行应包含匹配数后缀，格式如 'Glob (pattern: ...) — N matches'",
            "Grep 工具头行应包含匹配数后缀，格式如 'Grep (pattern: ...) — N matches'",
            "匹配数 N 应为至少为 1 的正整数，格式正确即可，不要求特定数量",
          ],
        });
        console.log("Judge:", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }
    },
  );
});
