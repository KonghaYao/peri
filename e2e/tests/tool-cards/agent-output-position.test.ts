/**
 * 工具卡片场景: Agent 工具返回值显示 + 嵌套工具位置
 *
 * 对应 issue:
 * - #1 2026-07-17-agent-tool-output-not-displayed
 *   Agent 工具完成后 ToolCard 下方 output_summary 为空
 * - #6 2026-07-12-agent-nested-toolcall-misplaced-into-history
 *   子工具卡片渲染在 Agent 卡片上方，未嵌套在内部
 *
 * 验证：
 * - Agent 工具完成后的 output_summary 非空（SubAgent 最终输出可见）
 * - 子工具调用卡片出现在 Agent 卡片下方（而非上方）
 *
 * 注意：explorer subagent 需要较长时间执行（30-60s），用长等待。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: agent output and nested position", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "Agent 工具完成后有输出且子工具卡片处于正确位置",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // 触发同步 explorer subagent，它会使用 Grep/Read 等工具
      await sendPrompt(
        tester,
        "请使用同步 explorer subagent，搜索 src 目录下是否有 TODO 注释",
      );

      // 等待 Agent 工具卡出现（explorer subagent 会先研究方案再执行工具）
      await tester.waitForText("Agent", {
        timeout: 60_000,
        interval: 1000,
      });
      // explorer 执行工具需要时间（Grep 等），等 10s 让它跑起来
      await tester.sleep(10000);

      const runningCapture = await takePeriSnapshot(
        tester,
        "agent-output-running",
      );

      // 等待 SubAgent 完成——explorer 执行需 30-60s，用足够长的固定等待
      // 已等 10s，再等 50s 确保完成
      await tester.sleep(50000);

      const doneCapture = await takePeriSnapshot(
        tester,
        "agent-output-done",
      );

      expect(runningCapture.text.length).toBeGreaterThan(50);
      expect(doneCapture.text.length).toBeGreaterThan(50);

      // Judge: 运行中 —— 子工具位置检查
      const r = await judge({
        ansiRaw: runningCapture.raw,
        criteria: [
          "屏幕应显示 SubAgent 正在工作的痕迹（如工具调用卡片或加载指示器）",
          "Agent 卡片内部应有具体的工具调用条目（如 ● Grep 或 ● Read，包含工具名称），而非仅展示空的 Agent 卡片外壳",
          "SubAgent 相关的内容（工具调用或状态信息）应出现在 Agent 卡片下方，而非上方历史消息中",
        ],
      });
      console.log("Judge (running):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 完成 —— Agent 输出 + 位置
      const r2 = await judge({
        ansiRaw: doneCapture.raw,
        criteria: [
          "Agent 工具卡片下方应有非空的输出摘要（output_summary），即 SubAgent 完成后的回复或搜索结论应可见",
          "如果 SubAgent 已完成，应有关于 TODO 搜索结果的文字说明——而非空白内容",
          "消息区中不应出现子工具调用卡片飘到 Agent 卡片上方、混入更早历史消息的情况",
        ],
      });
      console.log("Judge (done):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);
    },
  );
});
