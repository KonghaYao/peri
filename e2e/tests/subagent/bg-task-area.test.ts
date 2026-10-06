/**
 * 场景测试: 后台任务展示栏（BgTaskArea）多阶段会话融合
 *
 * 融合原 3 个测试（同一 UI 区域 BgTaskArea）：
 * - bg-agent-task-area:   bg subagent（sleep 12s）运行 ◎ agent → 持久完成
 * - bg-shell-task-area:   bg shell（run_in_background sleep 20）运行 ◎ shell → 持久完成
 * - fork-bg-callback:     bg fork subagent（sleep 12s）运行 → 子会话终态 ACK
 *
 * 一次会话 3 个顺序阶段。运行态必须在 BgTaskArea 可见；完成边界读取实际
 * Peri Store 的直接父 required delivery、Satisfied batch 及子会话终态 ACK。
 * 内部任务 reminder 不渲染，既不要求可见内部消息，也不以父回复猜测完成。
 */
import { describe, it, expect, afterEach } from "vitest";
import { createE2EModelHome, launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { captureBgStageBoundary, completedBgStage } from "../../helpers/bg-task-durable-boundary.js";
import type { TmuxTester } from "tui-tester";

describe("subagent: bg task area (merged)", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await takePeriSnapshot(tester, "bg-task-final-before-stop").catch(() => {});
      await tester.sendKey("end", { ctrl: true }).catch(() => {});
      await takePeriSnapshot(tester, "bg-task-final-after-ctrl-end").catch(() => {});
      await tester.stop().catch(() => {});
    }
  });

  it(
    "bg subagent / bg shell / bg fork 运行期间展示栏可见，完成后 ✔",
    { timeout: 600_000 },
    async () => {
      const home = createE2EModelHome();
      tester = await launchPeri({ env: { HOME: home } });
      const agentBoundary = captureBgStageBoundary(home);

      // ── 阶段 1：bg subagent（sleep 12s）──
      await sendPrompt(
        tester,
        "请调用 Agent 工具，参数必须包含 subagent_type=general-purpose、run_in_background=true；让它先用 Bash sleep 12，再返回 hello。不要使用同步 Agent。",
      );

      // 等待派发完成：BgTaskArea 出现 ● agent 运行条目（思考 + 派发约 10-15s）
      await tester.waitFor(
        (screen) => /◎ agent/.test(screen),
        { timeout: 120_000, interval: 1000, message: "等待 bg subagent 运行条目超时" },
      );
      const agentRunning = await takePeriSnapshot(tester, "bg-task-agent-running");

      // 等持久化完成通知；✔ agent 只保留 3s，不能作为唯一完成屏障。
      try {
        await tester.waitFor(
          () => completedBgStage(home, agentBoundary, "subagent", "hello") !== undefined,
          { timeout: 180_000, interval: 1000, message: "等待 bg subagent 完成超时" },
        );
      } catch (error) {
        await takePeriSnapshot(tester, "bg-task-agent-timeout");
        throw error;
      }
      const agentDone = await takePeriSnapshot(tester, "bg-task-agent-done");
      const agentEvidence = completedBgStage(home, agentBoundary, "subagent", "hello");
      expect(agentEvidence?.childAdmissionId).toBeDefined();
      const shellBoundary = captureBgStageBoundary(home);

      // ── 阶段 2：bg shell（run_in_background sleep 20）──
      await sendPrompt(
        tester,
        '这是 E2E 测试。请直接调用 Bash 工具，参数必须为 {"command":"sleep 20 && printf E2E_BG_SHELL_DONE","run_in_background":true}；不得改为前台运行或只解释。调用后回复回执里的任务 id（mcp- 开头）。',
      );
      try {
        await tester.waitFor(
          (screen) => /◎ shell/.test(screen),
          { timeout: 120_000, interval: 500, message: "等待 bg shell 运行条目" },
        );
      } catch (error) {
        await takePeriSnapshot(tester, "bg-task-shell-running-timeout");
        throw error;
      }
      await tester.sleep(2000);
      const shellRunning = await takePeriSnapshot(tester, "bg-task-shell-running");

      // 等持久完成通知；✔ shell 只保留 3s，不能作为唯一完成屏障。
      try {
        await tester.waitFor(
          () => completedBgStage(home, shellBoundary, "shell", "E2E_BG_SHELL_DONE") !== undefined,
          { timeout: 90_000, interval: 1000, message: "等待 bg shell 完成超时" },
        );
      } catch (error) {
        await takePeriSnapshot(tester, "bg-task-shell-timeout");
        throw error;
      }
      const shellDone = await takePeriSnapshot(tester, "bg-task-shell-done");
      const shellEvidence = completedBgStage(home, shellBoundary, "shell", "E2E_BG_SHELL_DONE");
      expect(shellEvidence).toBeDefined();
      const forkBoundary = captureBgStageBoundary(home);

      // ── 阶段 3：bg fork subagent（sleep 5s）──
      // 阶段 1 的 ● agent 已消失（✔ 保留 3s），● agent 再次出现即本阶段 fork
      await sendPrompt(
        tester,
        '这是 E2E 测试。请直接调用 Agent 工具，参数必须包含 {"fork":true,"run_in_background":true,"prompt":"先用 Bash 执行 sleep 12，再输出 E2E_BG_FORK_DONE"}；不要使用同步 Agent。',
      );
      await tester.waitFor(
        (screen) => /◎ agent/.test(screen),
        { timeout: 120_000, interval: 1000, message: "等待 bg fork 运行条目超时" },
      );
      await tester.sleep(2000);
      const forkRunning = await takePeriSnapshot(tester, "bg-task-fork-running");

      // fork 完成：等待持久化的 Agent: fork 回调通知，避免错过短暂 ✔。
      try {
        await tester.waitFor(
          () => completedBgStage(home, forkBoundary, "subagent", "E2E_BG_FORK_DONE") !== undefined,
          { timeout: 180_000, interval: 1000, message: "等待 bg fork 完成超时" },
        );
      } catch (error) {
        await takePeriSnapshot(tester, "bg-task-fork-timeout");
        throw error;
      }
      const forkDone = await takePeriSnapshot(tester, "bg-task-fork-done");
      const forkEvidence = completedBgStage(home, forkBoundary, "subagent", "E2E_BG_FORK_DONE");
      expect(forkEvidence?.childAdmissionId).toBeDefined();

      expect(agentRunning.text.length).toBeGreaterThan(50);
      expect(agentDone.text.length).toBeGreaterThan(50);
      expect(shellRunning.text.length).toBeGreaterThan(50);
      expect(shellDone.text.length).toBeGreaterThan(50);
      expect(forkRunning.text.length).toBeGreaterThan(50);
      expect(forkDone.text.length).toBeGreaterThan(50);

      // 核心断言：bg shell 运行期间 BgTaskArea 必须显示 ◎ shell 运行条目
      // （回归：此前只在完成时注册，运行期间展示栏无条目）
      expect(shellRunning.text).toContain("◎");
      expect(shellRunning.text).toContain("shell");

      // 所有断言都绑定到上面的因果屏障，避免再用 LLM judge 重复猜测 UI 状态。
      expect(agentRunning.text).toMatch(/◎ agent/);
      expect(agentEvidence?.batchId).toBeDefined();
      expect(shellRunning.text).toMatch(/◎ shell/);
      expect(shellEvidence?.parentSessionId).toBe(agentEvidence?.parentSessionId);
      expect(forkRunning.text).toMatch(/◎ agent/);
      expect(forkEvidence?.parentSessionId).toBe(agentEvidence?.parentSessionId);
      expect(forkEvidence?.invocationId).not.toBe(agentEvidence?.invocationId);
    },
  );
});
