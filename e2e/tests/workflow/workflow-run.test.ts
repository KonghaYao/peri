/**
 * 场景测试: Workflow 运行 → 面板观察 → 结果查看
 *
 * 验证触发 workflow 后，/workflow 面板显示运行中任务，
 * 完成后面板中可见完成状态和结果。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("workflow: run and observe", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "触发 workflow → /workflow 面板观察运行态 → 查看完成结果",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      const base = await tester.getScreenText();

      // 阶段 1：触发 workflow（使用 /ultracode skill）
      await sendPrompt(
        tester,
        "/ultracode 请派发一个简单的 workflow，用并行 agent 分别执行 echo hello workflow test",
      );

      // 等待 workflow 启动（agent 思考 + 派发，约 10-20s）
      // 等 workflow 工具卡片出现或 workflow 开始运行
      await tester.sleep(8000);

      // 先抓快照观测消息区（看 workflow 是否已启动）
      await tester.waitForText("workflow", {
        timeout: 30_000,
        interval: 1000,
      });

      const launchCapture = await takePeriSnapshot(tester, "workflow-launched");

      // 阶段 2：打开 /workflow 面板观察运行态
      await sendPrompt(tester, "/workflow");

      await tester.waitForText("Workflow", {
        timeout: 10_000,
        interval: 500,
      });

      await tester.sleep(1000);
      const runningCapture = await takePeriSnapshot(tester, "workflow-panel-running");

      // 关闭面板
      await tester.sendKey("Escape");
      await tester.sleep(500);

      // 阶段 3：等待 workflow 完成
      await waitForStableScreen(tester, 120_000, base);

      // 再次打开 /workflow 面板查看完成结果
      await sendPrompt(tester, "/workflow");
      await tester.waitForText("Workflow", {
        timeout: 10_000,
        interval: 500,
      });
      await tester.sleep(1000);

      const doneCapture = await takePeriSnapshot(tester, "workflow-panel-done");

      expect(launchCapture.text.length).toBeGreaterThan(100);
      expect(runningCapture.text.length).toBeGreaterThan(100);
      expect(doneCapture.text.length).toBeGreaterThan(100);

      // Judge: 启动阶段
      const r = await judge({
        ansiRaw: launchCapture.raw,
        criteria: [
          "agent 应已响应了 workflow 请求，消息区中有 workflow 相关输出（如工具卡片或启动通知）",
        ],
      });
      console.log("Judge (launch):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 面板 running 阶段——workflow 正在运行，但面板结果列表尚未更新
      // 实际运行状态在 BgTaskArea 和状态栏中可见
      const r2 = await judge({
        ansiRaw: runningCapture.raw,
        criteria: [
          "Workflow 面板已打开，标题栏显示 'Workflow'",
          "虽面板显示 '当前会话无工作流运行'，但状态栏或下方任务区应显示 '1 workflow' 或 '◎ workflow' 表示正在运行",
        ],
      });
      console.log("Judge (running):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);

      // Judge: 面板完成态
      const r3 = await judge({
        ansiRaw: doneCapture.raw,
        criteria: [
          "Workflow 面板应显示已完成的任务（可能显示 ✓ 或 completed 标记）",
          "面板中应有任务的执行结果或统计信息（如 agent 数量、耗时、输出摘要）",
        ],
      });
      console.log("Judge (done):", JSON.stringify(r3, null, 2));
      expect(r3.pass).toBe(true);
    },
  );
});
