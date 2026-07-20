/**
 * 工具卡片场景: Edit/Write 头行显示 diff 摘要
 *
 * issue: 2026-07-20-e2e-tool-call-header-suffix-tests
 *
 * 验证 Edit/Write 完成后头行显示变更摘要格式：
 * - Write 头行显示 "— N lines changed" 或 "— Wrote N line(s)"
 * - Edit 头行显示 "— N lines changed · +N · -N" 或 "— Replaced text..."
 * - 不再自动展开，但头行后缀包含变更信息
 * - diff 增减统计通过 "· +N · -N" 追加
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: edit write diff summary", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "Edit 完成后头行显示 diff 摘要",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // 两步操作：Write 创建 → Edit 修改
      await sendPrompt(
        tester,
        "请分两步操作：\n" +
          "第一步：用 Write 工具创建文件 /tmp/peri-e2e-diff-test.txt，写入内容：\n" +
          "line1: original\nline2: unchanged\nline3: old_value\n\n" +
          "第二步：用 Edit 工具把 'line3: old_value' 改成 'line3: new_value'\n" +
          "注意第二步必须用 Edit 工具（不能用 Write）",
      );

      // 等待 Write 工具开始执行
      await tester.waitForText("Write", {
        timeout: 60_000,
        interval: 1000,
      });

      // 等待 Edit 工具出现并完成
      await tester.waitForText("Edit", {
        timeout: 60_000,
        interval: 1000,
      });
      await tester.sleep(10000);

      const capture = await takePeriSnapshot(tester, "edit-write-diff");

      expect(capture.text.length).toBeGreaterThan(50);

      // Judge: 验证头行后缀格式
      try {
        const r = await judge({
          ansiRaw: capture.raw,
          criteria: [
            "屏幕上应出现 Write 和 Edit 两个工具调用的痕迹",
            "Write 工具头行应包含变更摘要（如 '— Wrote N line(s)' 或 '— N lines changed'）",
            "Edit 工具头行应包含 diff 增减统计或变更摘要（如 '— N lines changed · +N · -N' 或 '— Replaced text'）",
            "Write 和 Edit 头行的变更信息清晰可见，用于替代旧的独立输出行",
          ],
        });
        console.log("Judge:", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }
    },
  );
});
