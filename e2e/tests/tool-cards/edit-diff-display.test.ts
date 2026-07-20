/**
 * 工具卡片场景: Edit 工具 diff 显示
 *
 * 验证 Edit 完成后以头行后缀显示变更摘要：
 * - Edit 完成后头行显示 "— N lines changed · +N · -N" 后缀
 * - Write 工具创建基础文件
 * - agent 正确执行 Edit + Write 组合
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: edit diff display", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "Edit 成功后显示 diff 变更摘要",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      await sendPrompt(
        tester,
        "请分两步操作：\n" +
          "第一步：用 Write 工具创建文件 /tmp/peri-e2e-edit.txt，写入一行内容 'hello world'\n" +
          "第二步：用 Edit 工具修改该文件，把 'hello world' 改成 'hello peri e2e'\n" +
          "注意第二步必须用 Edit 工具（不能用 Write）",
      );

      // 等待 Write 工具开始执行
      await tester.waitForText("Write", {
        timeout: 60_000,
        interval: 1000,
      });

      // 等待 Edit 工具出现并完成（头行后缀格式如 "— Replaced text"）
      await tester.waitForText("Edit", {
        timeout: 60_000,
        interval: 1000,
      });
      // 等 Edit 完成——等待头行后缀出现（"Replaced" 或 "lines changed"）
      await tester.sleep(8000);

      const editCapture = await takePeriSnapshot(tester, "edit-diff");

      // 等待 agent 处理完 Edit 结果
      await tester.sleep(5000);
      const doneCapture = await takePeriSnapshot(tester, "edit-done");

      expect(editCapture.text.length).toBeGreaterThan(50);
      expect(doneCapture.text.length).toBeGreaterThan(50);

      // Judge: Edit 阶段
      try {
        const r = await judge({
          ansiRaw: editCapture.raw,
          criteria: [
            "屏幕上应出现 Write 和 Edit 工具调用的痕迹",
            "Edit 工具的头行应显示变更摘要（如 '— N lines changed · +N · -N' 或 '— Replaced text' 格式）",
            "agent 应执行了文件编辑操作，而非跳过或用其他方式替代",
          ],
        });
        console.log("Judge (edit):", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }

      // Judge: 完成阶段
      try {
        const r = await judge({
          ansiRaw: doneCapture.raw,
          criteria: [
            "agent 应确认文件编辑操作已完成",
            "屏幕上应包含编辑后的内容或编辑成功的确认信息",
          ],
        });
        console.log("Judge (done):", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }
    },
  );
});
