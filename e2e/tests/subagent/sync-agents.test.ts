/**
 * 场景测试: 同步 SubAgent 消息区渲染
 *
 * 验证 agent 调用同步 subagent 时，消息区正确显示 Agent 工具卡片
 * （● Agent）、嵌套工具和完成状态标记。
 * 注意：subagent 内部有 sleep，需要足够长的等待时间。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

const promptMarker = "❯ 请使用同步 subagent";

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

function hasRunningNestedShell(section: string): boolean {
  return /^\s*● (?:Bash|Shell) \([^\n]+\)\s*\n\s*⎿ Running \(\d+s\)/m.test(
    section,
  );
}

function hasCompletedNestedShell(section: string): boolean {
  return /^\s*● (?:Bash|Shell) \([^\n]+\)\s*\n\s*⎿ (?:hello|\[Command completed with exit code 0\])\s*$/m.test(
    section,
  );
}

describe("subagent: sync agents", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "同步 subagent 出现 Agent 工具卡片、嵌套工具和完成状态",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // subagent.md 原始 prompt
      await sendPrompt(
        tester,
        "请使用同步 subagent 用 shell say hello，但是它要先 sleep 10s",
      );

      // 等待当前 prompt 对应的 Agent turn 内嵌 Shell/Bash 卡片真正进入 Running。
      await tester.waitFor(
        (screen) => {
          const turn = currentAgentTurn(screen);
          return turn !== undefined && !turn.completed && hasRunningNestedShell(turn.section);
        },
        {
          timeout: 120_000,
          interval: 1000,
          message: "等待同步 subagent 的嵌套工具进入运行态超时",
        },
      );
      const runningCapture = await takePeriSnapshot(tester, "sync-subagent-running");

      // 完成态必须属于当前 prompt 对应的 Agent turn，且该 turn 内的 Shell/Bash 已完成。
      await tester.waitFor(
        (screen) => {
          const turn = currentAgentTurn(screen);
          return (
            turn !== undefined &&
            turn.completed &&
            hasCompletedNestedShell(turn.section)
          );
        },
        {
          timeout: 120_000,
          interval: 1000,
          message: "等待同步 subagent 和主 turn 完成超时",
        },
      );
      await tester.sleep(1000);
      const doneCapture = await takePeriSnapshot(tester, "sync-subagent-done");

      expect(runningCapture.text.length).toBeGreaterThan(100);
      expect(doneCapture.text.length).toBeGreaterThan(100);

      // Judge: running 态（Agent 已派发，subagent 正在执行中）
      const r = await judge({
        ansiRaw: runningCapture.raw,
        criteria: [
          "消息区应有 Agent 工具调用卡片（● Agent + 任务描述，如 'sleep 10s then echo hello'）",
          "Agent 卡片内部应有 SubAgent 执行的工具调用卡片（如 ● Bash 或 ● Shell，包含命令参数），而非仅展示空的卡片容器",
          "系统应处于处理中状态（如 Shell 卡片显示 'Running'、底部有 Spinner 或加载指示器），表明 subagent 仍在运行",
        ],
      });
      console.log("Judge (running):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 完成态
      const r2 = await judge({
        ansiRaw: doneCapture.raw,
        criteria: [
          "Agent 工具卡片应已完成执行（✅ 完成标记或绿色 ●，不再显示 running/⏳ 状态）",
          "Agent 卡片区域内应保留 SubAgent 内部工具调用的痕迹（如 Bash/Shell 工具条目），而非完成后变成完全空白的卡片",
          "消息区应包含 SubAgent 的执行结果（如 Shell 的 'hello' 输出或完成摘要）",
        ],
      });
      console.log("Judge (done):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);
    },
  );
});
