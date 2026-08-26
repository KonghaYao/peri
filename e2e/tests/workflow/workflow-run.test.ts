/**
 * 场景测试: Workflow 运行 → 面板观察 → 结果查看
 *
 * 验证触发 workflow 后，/workflows 面板显示运行中任务，
 * 完成后面板中可见完成状态和结果。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
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
        "/ultracode 这是 E2E 测试。请立即且只调用一次 Workflow 工具，不得先试错或只解释。" +
        "script 参数必须等价于以下顶层脚本：" +
        "export const meta = { name: 'e2e-run-observe', description: 'E2E workflow panel observation' } " +
        "phase('Run') " +
        "const results = await parallel([" +
        "() => agent('只用 Bash 执行 echo hello-workflow-a', { label: 'agent-a' })," +
        "() => agent('只用 Bash 执行 echo hello-workflow-b', { label: 'agent-b' })" +
        "]) " +
        "log(JSON.stringify(results))",
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
      expect(launchCapture.text).toMatch(
        /Workflow 'e2e-run-observe' completed\. \(/,
      );
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
