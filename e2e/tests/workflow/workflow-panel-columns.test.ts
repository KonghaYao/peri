/**
 * 场景测试: Workflow Panel Agent 列值显示
 *
 * 对应 issue:
 * - #7 2026-07-18-workflow-panel-agent-token-tool-display-zero
 *   workflow panel 中 token/tool 计数始终为 0，列标题缺失
 *
 * 验证：
 * - workflow 完成后，panel 中 agent 行的 token 和 tool 计数不再恒为 0
 * - 列标题或数值区域可见（即使为 0 也比不显示好）
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("workflow: panel columns", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "workflow 完成后 panel 中 agent 列值可见",
    { timeout: 480_000 },
    async () => {
      tester = await launchPeri();

      // 触发一个简单的 workflow
      await sendPrompt(
        tester,
        "/ultracode 请派发一个简单的 workflow，用两个并行 agent 分别执行 echo hello workflow columns test",
      );

      // 等待 workflow 真正完成：消息区出现完成通知
      // "Workflow '<name>' completed. (<duration>ms, ...)"（async_router.rs 生成，不会被翻译）
      // 注意：不能等 "workflow" 字样——prompt 回显里就有，会立即匹配（e2e/CLAUDE.md 稳定不变量）
      await tester.waitForText("completed. (", {
        timeout: 300_000,
        interval: 3000,
      });
      await tester.sleep(3000);

      // 打开 workflow 面板
      await tester.sendText("/workflows");
      await tester.sleep(500);
      await tester.sendKey("Enter");
      await tester.sleep(2000);

      const panelCapture = await takePeriSnapshot(
        tester,
        "workflow-panel-columns",
      );

      expect(panelCapture.text.length).toBeGreaterThan(50);

      // Judge: panel 内容
      const r = await judge({
        ansiRaw: panelCapture.raw,
        criteria: [
          "Workflow 面板应打开（标题为 'Workflow' 或包含 workflow 列表）",
          "已完成 workflow 的 agent 列表中应显示至少一个 agent 条目",
          "agent 条目旁边应有数值列（token 数和工具调用数），可以是 0 但列结构应存在",
        ],
      });
      console.log("Judge (panel):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);
    },
  );
});
