/**
 * 工具卡片场景: 后台 shell 任务区域（BgTaskArea）
 *
 * 验证 run_in_background 的 Bash 任务在运行期间显示在
 * status 下方展示栏（● shell desc · elapsed），完成后变 ✔ 并消失。
 *
 * 回归：bg shell 此前只在完成时注册（BgTaskStarted 与 BgTaskCompleted
 * 几乎同时发出），任务运行期间 BgTaskArea 无条目、status 栏无计数。
 *
 * 注意：不能等待屏幕稳定——bg shell 运行期间画面本来就要变化
 * （耗时递增），沿用固定等待。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: bg shell task area", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "run_in_background 运行期间 BgTaskArea 显示 ● shell，完成后消失",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      await sendPrompt(
        tester,
        "请使用 Bash 工具的 run_in_background 参数在后台运行 sleep 20，然后告诉我 task_id",
      );

      // 等待 agent 返回 task_id（shell-xxxx；prompt 回显与工具参数中都不含此文本）
      await tester.waitForText("shell-", {
        timeout: 60_000,
        interval: 1000,
      });

      // 运行期间（sleep 20 进行中）抓屏：BgTaskArea 应显示 ● shell
      await tester.sleep(2000);
      const runningCapture = await takePeriSnapshot(tester, "bg-shell-running");

      // 等待完成通知（i18n app-note-bg-task-completed）
      await tester.waitForText("已完成", {
        timeout: 60_000,
        interval: 2000,
      });
      await tester.sleep(2000);

      const doneCapture = await takePeriSnapshot(tester, "bg-shell-done");

      expect(runningCapture.text.length).toBeGreaterThan(50);
      expect(doneCapture.text.length).toBeGreaterThan(50);

      // 核心断言：运行期间 BgTaskArea 展示栏必须有 ● shell 条目
      expect(runningCapture.text).toContain("●");
      expect(runningCapture.text).toContain("shell");

      // Judge: 运行中展示栏可见
      const r = await judge({
        ansiRaw: runningCapture.raw,
        criteria: [
          "状态栏下方的后台任务展示栏应显示 shell 任务行（● shell 开头，含运行耗时）",
          "屏幕应显示后台任务已启动（含 shell- 开头的 task_id）",
        ],
      });
      console.log("Judge (running):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 完成态
      const r2 = await judge({
        ansiRaw: doneCapture.raw,
        criteria: [
          "后台 shell 任务应已完成（完成通知出现，● shell 条目不再处于运行态）",
          "agent 应已收到后台任务结果并给出回复",
        ],
      });
      console.log("Judge (done):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);
    },
  );
});
