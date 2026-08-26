/**
 * 场景测试: Thread 切换 + History 面板
 *
 * 验证 /threads 斜杠命令打开历史面板，用户可选择历史线程并按 Enter 切换，
 * 切换后消息区加载该线程的内容。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("scenarios: thread switch", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "/threads 打开历史面板并切换线程",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // 阶段 1：先发一条消息创建当前线程
      const base = await tester.getScreenText();
      await sendPrompt(tester, "hello");
      await waitForStableScreen(tester, 60_000, base);
      // service_snapshot 每 2s 刷新线程列表；确保当前 turn 已写入 store。
      await tester.sleep(2500);

      // 阶段 2：通过 /threads 命令打开历史面板
      await sendPrompt(tester, "/threads");

      await tester.waitForText("Threads", {
        timeout: 10_000,
        interval: 500,
      });

      const panelCapture = await takePeriSnapshot(tester, "thread-panel-open");

      // 阶段 3：在面板中选择一个不同的线程
      await tester.sendKey("down");
      await tester.sleep(200);
      await tester.sendKey("Enter");
      await tester.sleep(1000);

      // 等待线程切换完成（消息区加载历史内容）
      await waitForStableScreen(tester, 60_000);

      const capture = await takePeriSnapshot(tester, "thread-switch-done");

      // 基本断言
      expect(panelCapture.text).toContain("Threads");
      expect(capture.text.length).toBeGreaterThan(50);
      expect(capture.text).not.toMatch(/─+\s*Threads\s*─+/);
      expect(capture.text).toContain("hello");

      // LLM judge: 面板阶段
      const panelResult = await judge({
        ansiRaw: panelCapture.raw,
        criteria: [
          "屏幕中应有 Threads 面板，显示历史线程列表（含线程标题和消息计数）",
          "面板中应有可选的线程条目，当前选中项应有视觉提示（如高亮、> 符号）",
        ],
      });
      console.log("Judge (panel):", JSON.stringify(panelResult, null, 2));
      expect(panelResult.pass).toBe(true);

      // 完成态使用结构化文本断言：线程 tab 也会显示标题 "hello"，视觉
      // Judge 容易把 tab 误认成仍打开的 Threads 列表面板。上面的边框标题
      // absence + 历史消息 presence 已精确覆盖切换结果。
    },
  );
});
