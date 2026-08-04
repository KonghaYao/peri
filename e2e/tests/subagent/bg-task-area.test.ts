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
import { judge } from "../../helpers/judge.js";
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

      // ── 阶段 1：bg subagent（sleep 3s）──
      await sendPrompt(
        tester,
        "请使用 bg subagent say hello，但是它要先 sleep 3s",
      );

      // 等待派发完成：BgTaskArea 出现 ● agent 运行条目（思考 + 派发约 10-15s）
      await tester.waitFor(
        (screen) => /◎ agent/.test(screen),
        { timeout: 60_000, interval: 1000, message: "等待 bg subagent 运行条目超时" },
      );
      await tester.sleep(2000);
      const agentRunning = await takePeriSnapshot(tester, "bg-task-agent-running");

      // 等待完成：● agent → ✔ agent（保留 3s，轮询 1s 可捕获）
      await tester.waitFor(
        (screen) => /✔ agent/.test(screen),
        { timeout: 60_000, interval: 1000, message: "等待 bg subagent 完成超时" },
      );
      const agentDone = await takePeriSnapshot(tester, "bg-task-agent-done");

      // ── 阶段 2：bg shell（run_in_background sleep 20）──
      await sendPrompt(
        tester,
        "请使用 Bash 工具的 run_in_background 参数在后台运行 sleep 20，然后告诉我 task_id",
      );
      await tester.waitForText("shell-", {
        timeout: 60_000,
        interval: 1000,
      });
      await tester.sleep(2000);
      const shellRunning = await takePeriSnapshot(tester, "bg-task-shell-running");

      // 等 ✔ shell（sleep 20 完成，通知含独特文本 "bg-shell"）
      await tester.waitFor(
        (screen) => /✔ shell/.test(screen),
        { timeout: 90_000, interval: 1000, message: "等待 bg shell 完成超时" },
      );
      const shellDone = await takePeriSnapshot(tester, "bg-task-shell-done");

      // ── 阶段 3：bg fork subagent（sleep 5s）──
      // 阶段 1 的 ● agent 已消失（✔ 保留 3s），● agent 再次出现即本阶段 fork
      await sendPrompt(
        tester,
        "请使用 bg fork subagent say hello，但是它要先 sleep 5s",
      );
      await tester.waitFor(
        (screen) => /◎ agent/.test(screen),
        { timeout: 60_000, interval: 1000, message: "等待 bg fork 运行条目超时" },
      );
      await tester.sleep(2000);
      const forkRunning = await takePeriSnapshot(tester, "bg-task-fork-running");

      // fork 完成：● agent 消失（变 ✔）
      await tester.waitFor(
        (screen) => !/◎ agent/.test(screen),
        { timeout: 90_000, interval: 1000, message: "等待 bg fork 完成超时" },
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

      // Judge: bg subagent 运行 + 完成
      const r1 = await judge({
        ansiRaw: agentRunning.raw,
        criteria: [
          "系统应处于处理中状态：应有思考块（如 '思考了 N 字符'）或底部有加载指示器",
          "输入提示应已提交（屏幕显示用户 prompt），agent 在准备或启动后台任务",
        ],
      });
      console.log("Judge (bg agent running):", JSON.stringify(r1, null, 2));
      expect(r1.pass).toBe(true);

      const r2 = await judge({
        ansiRaw: agentDone.raw,
        criteria: [
          "后台 agent 应已完成（✔ 标记、完成通知或状态栏 agent 计数归零）",
          "消息区应包含 SubAgent 的完成通知或执行结果",
        ],
      });
      console.log("Judge (bg agent done):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);

      // Judge: bg shell 运行期间展示栏可见
      const r3 = await judge({
        ansiRaw: shellRunning.raw,
        criteria: [
          "状态栏下方的后台任务展示栏应显示 shell 任务行（◎ shell 开头，含运行耗时）",
          "屏幕应显示后台任务已启动（含 shell- 开头的 task_id）",
        ],
      });
      console.log("Judge (bg shell running):", JSON.stringify(r3, null, 2));
      expect(r3.pass).toBe(true);

      // Judge: bg shell 完成
      const r4 = await judge({
        ansiRaw: shellDone.raw,
        criteria: [
          "后台 shell 任务应已完成（完成通知出现，● shell 条目不再处于运行态）",
          "agent 应已收到后台任务结果并给出回复",
        ],
      });
      console.log("Judge (bg shell done):", JSON.stringify(r4, null, 2));
      expect(r4.pass).toBe(true);

      // Judge: bg fork 完成回调
      const r5 = await judge({
        ansiRaw: forkDone.raw,
        criteria: [
          "后台 fork agent 应已完成（✔ 标记、完成通知或状态栏 agent 计数归零）",
          "消息区应出现 SubAgent 完成后的回调通知或结果（如 'hello' 或完成摘要）",
        ],
      });
      console.log("Judge (bg fork done):", JSON.stringify(r5, null, 2));
      expect(r5.pass).toBe(true);
    },
  );
});
