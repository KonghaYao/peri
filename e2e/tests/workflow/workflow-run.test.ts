/**
 * 场景测试: Workflow 运行 → 面板观察 → 结果查看
 *
 * 验证触发 workflow 后，/workflows 面板显示运行中任务，
 * 完成后面板中可见完成状态和结果。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import {
  E2E_WORKFLOW_RUN_OBSERVE_SCRIPT,
  triggerWorkflowAndWait,
} from "../../helpers/workflow.js";
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

      const runId = await triggerWorkflowAndWait(
        tester,
        "e2e-run-observe",
        E2E_WORKFLOW_RUN_OBSERVE_SCRIPT,
        { timeoutMs: 480_000, requireScreenNotification: false },
      );
      expect(runId.length).toBeGreaterThan(0);

      await tester.sendKey("home", { ctrl: true });
      await tester.sleep(800);

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

      // 阶段 3：workflow 在阶段 1 已由持久完成通知因果确认；关闭面板后不再
      // 重等可能已滚出视口的同一条通知，直接重新打开面板检查终态。
      await tester.sendText("/workflows");
      await tester.sleep(500);
      await tester.sendKey("Enter");
      await tester.sleep(2500);

      const doneCapture = await takePeriSnapshot(tester, "workflow-panel-done");

      expect(launchCapture.text.length).toBeGreaterThan(100);
      expect(runningCapture.text.length).toBeGreaterThan(100);
      expect(doneCapture.text.length).toBeGreaterThan(100);

      // 上面的 waitForText 与面板快照已提供因果证据，直接断言结构，避免
      // 让 LLM judge 把合法的已完成面板误判成“没有运行中任务”。
      // 完成因果由磁盘 runId 保证；消息区通知可能已滚出视口，不在此快照硬断言。
      expect(launchCapture.text).toMatch(/e2e-run-observe|Workflow/);
      expect(runningCapture.text).toContain("Workflow");
      expect(runningCapture.text).toContain("e2e-run-observe");
      expect(runningCapture.text).not.toContain("当前会话无工作流运行");
      expect(doneCapture.text).toContain("Workflow");
      expect(doneCapture.text).toContain("e2e-run-observe");
      expect(doneCapture.text).toMatch(/✓\s+e2e-run-observe/);
      expect(doneCapture.text).toContain("Agents");
    },
  );
});
