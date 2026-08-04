/**
 * 场景测试: 同步 SubAgent 卡片渲染（多阶段会话融合）
 *
 * 融合原 2 个测试：
 * - internal-toolcards-visibility: 同步 subagent（echo 瞬间完成）内部工具卡片
 *   非空壳、完成后痕迹保留
 * - sync-agents: 同步 subagent（sleep 10s）running 态嵌套工具 + 完成态
 *
 * 一次会话 2 个顺序阶段，用 prompt 前缀定位当前 Agent turn。
 */
import { describe, it, expect, afterEach } from "vitest";
import {
  launchPeri,
  sendPrompt,
  takePeriSnapshot,
  waitForStableScreen,
} from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

const STAGE = {
  echo: "请使用同步 subagent 执行 shell 命令 echo hello-subagent-internal",
  sleep: "请使用同步 subagent 用 shell say hello",
} as const;

interface AgentTurn {
  section: string;
  completed: boolean;
}

/** 当前 prompt 之后、第一个 "● Agent (" 卡片到 "处理耗时" footer 的区段 */
function currentAgentTurn(screen: string, promptMarker: string): AgentTurn | undefined {
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

describe("subagent: sync agent cards (merged)", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "同步 subagent 内部工具卡片非空壳，且 running/完成态渲染正确",
    { timeout: 600_000 },
    async () => {
      tester = await launchPeri();

      // ── 阶段 1：echo subagent —— 内部工具卡片非空壳 + 完成后痕迹保留 ──
      const base = await tester.getScreenText();
      await sendPrompt(tester, STAGE.echo);

      // 等 SubAgent 执行工具（● Bash/● Shell 条目出现）
      await tester.waitFor(
        (screen) => /● (?:Bash|Shell)/.test(screen),
        {
          timeout: 60_000,
          interval: 1000,
          message: "等待 SubAgent 的工具卡片出现超时",
        },
      );
      const runningCapture = await takePeriSnapshot(
        tester,
        "sync-subagent-echo-running",
      );

      await waitForStableScreen(tester, 120_000, base);
      const echoDoneCapture = await takePeriSnapshot(
        tester,
        "sync-subagent-echo-done",
      );

      expect(runningCapture.text.length).toBeGreaterThan(50);
      expect(echoDoneCapture.text.length).toBeGreaterThan(50);

      // Judge: 内部工具卡片非空壳
      const r1 = await judge({
        ansiRaw: runningCapture.raw,
        criteria: [
          "消息区中应出现 SubAgent 内部工具调用的具体卡片（如 ● Bash 或 ● Shell 或 ● Grep），包含工具名称和参数摘要",
          "内部工具卡片不应只是空的 Agent 外壳——Agent 卡片区域内应有具体的工具条目（每行以 ● 开头、后跟工具名）",
          "这些内部工具卡片应出现在 Agent 卡片区域内部，而非散落在消息区其他位置",
        ],
      });
      console.log("Judge (echo running):", JSON.stringify(r1, null, 2));
      expect(r1.pass).toBe(true);

      // Judge: 完成后工具卡片痕迹保留 + 结果可见
      const r2 = await judge({
        ansiRaw: echoDoneCapture.raw,
        criteria: [
          "SubAgent 完成后，Agent 卡片区域内仍应保留工具调用的痕迹（如工具名称、执行计数、或 ● 标记），而非完全是空白的卡片容器",
          "Agent 工具卡片下方应有 SubAgent 完成后的结果摘要——可以是文本总结、文件列表或统计信息，不应完全空白",
          "不应出现子工具调用卡片飘到 Agent 卡片上方或混入历史消息的情况",
        ],
      });
      console.log("Judge (echo done):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);

      // ── 阶段 2：sleep 10s subagent —— running 嵌套工具 + 完成态 ──
      await sendPrompt(tester, STAGE.sleep + "，但是它要先 sleep 10s");

      // 等待当前 turn 内嵌 Shell/Bash 卡片真正进入 Running
      await tester.waitFor(
        (screen) => {
          const turn = currentAgentTurn(screen, STAGE.sleep);
          return (
            turn !== undefined &&
            !turn.completed &&
            /^\s*● (?:Bash|Shell) \([^\n]+\)\s*\n\s*⎿ Running \(\d+s\)/m.test(
              turn.section,
            )
          );
        },
        {
          timeout: 120_000,
          interval: 1000,
          message: "等待同步 subagent 的嵌套工具进入运行态超时",
        },
      );
      const sleepRunningCapture = await takePeriSnapshot(
        tester,
        "sync-subagent-sleep-running",
      );

      // 完成态必须属于当前 prompt 对应的 Agent turn，且该 turn 内的 Shell/Bash 已完成
      await tester.waitFor(
        (screen) => {
          const turn = currentAgentTurn(screen, STAGE.sleep);
          return (
            turn !== undefined &&
            turn.completed &&
            /^\s*● (?:Bash|Shell) \([^\n]+\)\s*\n\s*⎿ (?:hello|\[Command completed with exit code 0\])\s*$/m.test(
              turn.section,
            )
          );
        },
        {
          timeout: 120_000,
          interval: 1000,
          message: "等待同步 subagent 和主 turn 完成超时",
        },
      );
      await tester.sleep(1000);
      const sleepDoneCapture = await takePeriSnapshot(
        tester,
        "sync-subagent-sleep-done",
      );

      expect(sleepRunningCapture.text.length).toBeGreaterThan(100);
      expect(sleepDoneCapture.text.length).toBeGreaterThan(100);

      // Judge: running 态
      const r3 = await judge({
        ansiRaw: sleepRunningCapture.raw,
        criteria: [
          "消息区应有 Agent 工具调用卡片（● Agent + 任务描述）",
          "Agent 卡片内部应有 SubAgent 执行的工具调用卡片（如 ● Bash 或 ● Shell，包含命令参数），而非仅展示空的卡片容器",
          "系统应处于处理中状态（如 Shell 卡片显示 'Running'、底部有 Spinner 或加载指示器），表明 subagent 仍在运行",
        ],
      });
      console.log("Judge (sleep running):", JSON.stringify(r3, null, 2));
      expect(r3.pass).toBe(true);

      // Judge: 完成态
      const r4 = await judge({
        ansiRaw: sleepDoneCapture.raw,
        criteria: [
          "Agent 工具卡片应已完成执行（✅ 完成标记或绿色 ●，不再显示 running/⏳ 状态）",
          "Agent 卡片区域内应保留 SubAgent 内部工具调用的痕迹（如 Bash/Shell 工具条目），而非完成后变成完全空白的卡片",
          "消息区应包含 SubAgent 的执行结果（如 Shell 的 'hello' 输出或完成摘要）",
        ],
      });
      console.log("Judge (sleep done):", JSON.stringify(r4, null, 2));
      expect(r4.pass).toBe(true);
    },
  );
});
