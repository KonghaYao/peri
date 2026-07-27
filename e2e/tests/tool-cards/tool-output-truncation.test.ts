/**
 * 工具卡片场景: 工具输出截断
 *
 * 验证 Read 大文件时输出被截断的显示：
 * - Read 折叠态头行显示 "— N lines" 后缀（零内容行）
 * - 展开后最多 4 行 × 400 字符
 * - 超出部分显示截断提示（如 "… N more lines"）
 * - agent 能基于截断输出继续工作
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: output truncation", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "Read 大文件时输出被截断",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // Cargo.lock 通常很大，输出会被截断
      await sendPrompt(
        tester,
        "请用 Read 工具读取 Cargo.lock 文件的内容",
      );

      // 等待 Read 工具被调用
      await tester.waitForText("Read", {
        timeout: 60_000,
        interval: 1000,
      });
      await tester.sleep(3000);

      const readCapture = await takePeriSnapshot(
        tester,
        "tool-truncation-read",
      );

      // 等待 agent 处理完
      await tester.sleep(5000);
      const afterCapture = await takePeriSnapshot(
        tester,
        "tool-truncation-after",
      );

      expect(readCapture.text.length).toBeGreaterThan(50);
      expect(afterCapture.text.length).toBeGreaterThan(50);

      // Judge: Read 阶段
      const r = await judge({
        ansiRaw: readCapture.raw,
        criteria: [
          "屏幕上应出现 Read 工具调用的痕迹（如 'Read' 字样）",
          "Read 工具的头行应显示行数摘要（如 '— N lines' 格式），表明文件已被读取",
          "输出应被截断——不应显示完整的 Cargo.lock（该文件通常 5000+ 行）",
        ],
      });
      console.log("Judge (read):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 完成阶段
      const r2 = await judge({
        ansiRaw: afterCapture.raw,
        criteria: [
          "agent 应基于读取的内容给出分析或总结",
          "agent 能基于可见内容给出有价值的分析，即使提到了截断也不影响分析质量",
        ],
      });
      console.log("Judge (after):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);
    },
  );
});
