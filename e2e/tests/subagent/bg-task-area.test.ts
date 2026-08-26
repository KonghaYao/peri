/**
 * 场景测试: 后台任务展示栏（BgTaskArea）多阶段会话融合
 *
 * 融合原 3 个测试（同一 UI 区域 BgTaskArea）：
 * - bg-agent-task-area:   bg subagent（sleep 3s）运行 ● agent → 完成 ✔ agent
 * - bg-shell-task-area:   bg shell（run_in_background sleep 20）运行 ● shell → ✔ shell
 * - fork-bg-callback:     bg fork subagent（sleep 5s）运行 → 完成回调通知
 *
 * 一次会话 3 个顺序阶段。阶段边界用 BgTaskArea 条目状态：
 * ● shell 是 bg shell 独有；● agent 在阶段 1 完成（✔ 3s 后消失）后
 * 再次出现即阶段 3 的 fork。完成通知均为 "Agent: fork"，不可作边界。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import type { TmuxTester } from "tui-tester";

describe("subagent: bg task area (merged)", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "bg subagent / bg shell / bg fork 运行期间展示栏可见，完成后 ✔",
    { timeout: 600_000 },
    async () => {
      tester = await launchPeri();

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
      await tester.waitFor(
        (screen) => /后台任务 bg-[^\]]+ 已完成[\s\S]*Agent: general-purpose/.test(screen),
        { timeout: 180_000, interval: 1000, message: "等待 bg subagent 完成超时" },
      );
      const agentDone = await takePeriSnapshot(tester, "bg-task-agent-done");

      // ── 阶段 2：bg shell（run_in_background sleep 20）──
      await sendPrompt(
        tester,
        '这是 E2E 测试。请直接调用 Bash 工具，参数必须为 {"command":"sleep 20","run_in_background":true}；不得改为前台运行或只解释。调用后回复返回的 shell- 开头 task_id。',
      );
      await tester.waitFor(
        // task_id 由主模型稍后写入 transcript；20s shell 可能在模型回复前已
        // 完成，因此不能要求短暂运行条目与 task_id 同屏。
        (screen) => /◎ shell/.test(screen),
        {
          timeout: 120_000,
          interval: 500,
          message: "等待 bg shell 运行条目",
        },
      );
      await tester.sleep(2000);
      const shellRunning = await takePeriSnapshot(tester, "bg-task-shell-running");

      // 等持久完成通知；✔ shell 只保留 3s，不能作为唯一完成屏障。
      await tester.waitFor(
        (screen) =>
          /后台任务 shell-[^\]]+ 已完成[\s\S]*Agent: bg-shell/.test(screen),
        { timeout: 90_000, interval: 1000, message: "等待 bg shell 完成超时" },
      );
      const shellDone = await takePeriSnapshot(tester, "bg-task-shell-done");

      // ── 阶段 3：bg fork subagent（sleep 5s）──
      // 阶段 1 的 ● agent 已消失（✔ 保留 3s），● agent 再次出现即本阶段 fork
      await sendPrompt(
        tester,
        '这是 E2E 测试。请直接调用 Agent 工具，参数必须包含 {"fork":true,"run_in_background":true,"prompt":"先用 Bash sleep 12，再返回 hello"}；不要使用同步 Agent。',
      );
      await tester.waitFor(
        (screen) => /◎ agent/.test(screen),
        { timeout: 120_000, interval: 1000, message: "等待 bg fork 运行条目超时" },
      );
      await tester.sleep(2000);
      const forkRunning = await takePeriSnapshot(tester, "bg-task-fork-running");

      // fork 完成：等待持久化的 Agent: fork 回调通知，避免错过短暂 ✔。
      await tester.waitFor(
        (screen) => /后台任务 bg-[^\]]+ 已完成[\s\S]*Agent: fork/.test(screen),
        { timeout: 180_000, interval: 1000, message: "等待 bg fork 完成超时" },
      );
      const forkDone = await takePeriSnapshot(tester, "bg-task-fork-done");

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
      expect(agentDone.text).toMatch(
        /后台任务 bg-[^\]]+ 已完成[\s\S]*Agent: general-purpose/,
      );
      expect(shellRunning.text).toMatch(/◎ shell/);
      expect(shellDone.text).toMatch(
        /后台任务 shell-[^\]]+ 已完成[\s\S]*Agent: bg-shell/,
      );
      expect(forkRunning.text).toMatch(/◎ agent/);
      expect(forkDone.text).toMatch(
        /后台任务 bg-[^\]]+ 已完成[\s\S]*Agent: fork/,
      );
    },
  );
});
