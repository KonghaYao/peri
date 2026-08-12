/**
 * 冒烟场景: 40×8 极小视口（§11 响应式降级，Slice 1）
 *
 * 高度 < 12：隐藏 session title 与 key hints；< 8：composer ≤2 行。
 * 40×8 下 transcript 至少保留 3 行、composer 可操作、无 panic。
 *
 * [Slice 5] 新增场景（§15 golden scene：40×8 viewport）。
 * 依赖 Slice 1 的 `layout_plan` 高度断点（status 2 行 + composer 3 行
 * → transcript = 3 行）。
 *
 * 等待策略（Slice 5 修复记录）：40×8 下 3 行视口只显示最新内容（footer
 * 处理耗时行），user bubble "hi" 与回答文本在视口上方（顶部 ▲ 指示）——
 * `waitForText("hi")` 必然超时。改为等待 turn 完成标志（footer 处理耗时）。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("smoke: 40x8 minimal viewport", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "40×8 下正常提问回答（transcript 保留、无 panic）",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri({ size: { cols: 40, rows: 8 } });

      await sendPrompt(tester, "hi");

      // 等待 turn 完成（footer 处理耗时；中英文环境各兜底）。
      // 40×8 下 user bubble 被滚出 3 行视口——不能等 "hi"。
      try {
        await tester.waitForText("处理耗时", {
          timeout: 120_000,
          interval: 1000,
        });
      } catch {
        await tester.waitForText("Brewed for", {
          timeout: 60_000,
          interval: 1000,
        });
      }
      await tester.sleep(1500);

      const capture = await takePeriSnapshot(tester, "smoke-40x8");
      expect(capture.text.length).toBeGreaterThan(10);

      // footer（turn 完成标志）必须可见——硬断言（40×8 下 3 行视口只显示
      // 最新内容：滚动指示 + footer；回答文本在视口上方是 §11 设计行为）
      const hasFooter =
        capture.text.includes("处理耗时") || capture.text.includes("Brewed for");
      expect(hasFooter, `footer 处理耗时可见：${capture.text.slice(-300)}`).toBe(
        true,
      );

      const r = await judge({
        ansiRaw: capture.raw,
        criteria: [
          "40×8 极小终端下 TUI 不崩溃、屏幕非空白",
          "消息区可见（处理耗时 footer 行或滚动指示符）",
          "底部有输入区域（composer 的 ❯ 提示符可见）",
        ],
      });
      expect(r.pass, `40×8 冒烟通过`).toBe(true);
    },
  );
});
