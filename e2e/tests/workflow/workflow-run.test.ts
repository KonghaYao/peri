/**
 * 场景测试: Workflow 运行 → 面板观察 → 结果查看
 *
 * 验证触发 workflow 后，/workflows 面板显示运行中任务，
 * 完成后面板中可见完成状态和结果。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
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
    "触发 workflow → /workflows 面板观察运行态 → 查看完成结果",
    { timeout: 480_000 },
    async () => {
      tester = await launchPeri();

      // 阶段 1：触发 workflow（使用 /ultracode skill）
      await sendPrompt(
        tester,
        "/ultracode 请派发一个简单的 workflow，用并行 agent 分别执行 echo hello workflow test",
      );

      // 等 workflow 真正完成：消息区出现完成通知
      // "Workflow '<name>' completed. (<duration>ms, ...)"（async_router.rs 生成，不会被翻译）
      // 注意：不能等启动信号（WorkflowTool 返回 "Results will be saved..."）——agent 的
      // 长输出会把消息区中部的工具卡片滚出屏幕，不可见；完成通知在消息区末尾，必定可见。
      // 也不能等 "workflow" 字样——prompt 回显里就有，会立即匹配（e2e/CLAUDE.md 稳定不变量）
      await tester.waitForText("completed. (", {
        timeout: 300_000,
        interval: 3000,
      });
      await tester.sleep(3000);

      const launchCapture = await takePeriSnapshot(tester, "workflow-launched");

      // 阶段 2：打开 /workflows 面板观察运行态
      // 不能用 waitForText("Workflow") 判断面板已打开——消息区里 WorkflowTool
      // 卡片 "Workflow 'x' started" 会先匹配；面板渲染较快，固定等待即可
      await sendPrompt(tester, "/workflows");
      await tester.sleep(2500);

      const runningCapture = await takePeriSnapshot(tester, "workflow-panel-running");

      // 关闭面板
      await tester.sendKey("Escape");
      await tester.sleep(500);

      // 阶段 3：等待 workflow 完成通知
      // "Workflow '<name>' completed. (<duration>ms, ...)"（async_router.rs 生成，不会被翻译）
      await tester.waitForText("completed. (", {
        timeout: 120_000,
        interval: 3000,
      });
      await tester.sleep(2000);

      // 再次打开 /workflows 面板查看完成结果（同上：固定等待面板渲染）
      await tester.sendText("/workflows");
      await tester.sleep(500);
      await tester.sendKey("Enter");
      await tester.sleep(2500);

      const doneCapture = await takePeriSnapshot(tester, "workflow-panel-done");

      expect(launchCapture.text.length).toBeGreaterThan(100);
      expect(runningCapture.text.length).toBeGreaterThan(100);
      expect(doneCapture.text.length).toBeGreaterThan(100);

      // Judge: 启动阶段
      const r = await judge({
        ansiRaw: launchCapture.raw,
        criteria: [
          "消息区中应有 workflow 相关输出（如 'Workflow ... completed' 完成通知或 workflow 摘要）",
        ],
      });
      console.log("Judge (launch):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 面板 running 阶段——workflow 已启动，面板中应有任务条目
      // （workflow 是 fire-and-forget，echo 任务几秒内完成，运行中或已完成均可）
      const r2 = await judge({
        ansiRaw: runningCapture.raw,
        criteria: [
          "Workflow 面板已打开，标题栏显示 'Workflow'",
          "面板中应显示 workflow 任务条目：run tab 显示 workflow 名称（可带运行中/✓ 完成标记），或 agent 行列表；不应显示 '当前会话无工作流运行'",
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
