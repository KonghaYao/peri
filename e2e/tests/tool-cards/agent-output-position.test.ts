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

const promptMarker = "❯ 请使用同步 explorer subagent（quick thoroughness）";

interface AgentTurn {
  section: string;
  completed: boolean;
}

function currentAgentTurn(screen: string): AgentTurn | undefined {
  const promptStart = screen.lastIndexOf(promptMarker);
  const agentStart =
    promptStart >= 0 ? screen.indexOf("● Agent (", promptStart) : -1;
  if (agentStart < 0) {
    return undefined;
  }

  const turnEnd = screen.indexOf("处理耗时", agentStart);
  return {
    section: screen.slice(agentStart, turnEnd >= agentStart ? turnEnd : undefined),
    completed: turnEnd >= agentStart,
  };
}

function hasNestedGrep(section: string): boolean {
  return /^\s*● Grep \([^\n]+\)/m.test(section);
}

function hasRunningAgent(section: string): boolean {
  return /^\s*⎿ \d+ tool calls, running \d+s/m.test(section);
}

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

      // 触发受限的同步 explorer subagent：保留嵌套工具卡片和结果摘要覆盖，
      // 避免全量 src 搜索导致模型长时间重复工具调用。
      await sendPrompt(
        tester,
        "请使用同步 explorer subagent（quick thoroughness），仅在 peri-tui/src/kit 中用一次 Grep 搜索 TODO 注释；完成后立即用一句中文总结，不要做额外搜索或读取。",
      );

      // 等待当前 prompt 对应的 Agent turn 中 Grep 实际嵌套出现，再抓运行态。
      await tester.waitFor(
        (screen) => {
          const turn = currentAgentTurn(screen);
          return (
            turn !== undefined &&
            !turn.completed &&
            hasRunningAgent(turn.section) &&
            hasNestedGrep(turn.section)
          );
        },
        {
          timeout: 60_000,
          interval: 1000,
          message: "等待 explorer Agent 的嵌套 Grep 运行态超时",
        },
      );

      const runningCapture = await takePeriSnapshot(
        tester,
        "agent-output-running",
      );

      // 只有当前 prompt 对应的 Agent turn 完成后，才能检查 output_summary。
      await tester.waitFor(
        (screen) => {
          const turn = currentAgentTurn(screen);
          return (
            turn !== undefined &&
            turn.completed &&
            hasNestedGrep(turn.section) &&
            !turn.section.includes("running")
          );
        },
        {
          timeout: 180_000,
          interval: 2000,
          message: "等待同步 explorer SubAgent 和主 turn 完成超时",
        },
      );
      await tester.sleep(1000);

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
