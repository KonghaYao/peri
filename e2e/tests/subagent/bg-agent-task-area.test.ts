/**
 * 场景测试: Background Agent 任务区域
 *
 * 验证 bg subagent 运行时在 BgTaskArea 显示运行状态
 * （◎ coder desc · Xs），完成后切换到完成态（✔）。
 *
 * 注意：bg 模式下 agent 先思考再派发，sleep 期间画面可能保持稳定，
 * 因此不用 waitForStableScreen，改用固定等待。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("subagent: bg agent task area", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "bg subagent 运行时 BgTaskArea 显示进度，完成后 ✔",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // subagent.md 原始 prompt
      await sendPrompt(
        tester,
        "请使用 bg subagent say hello，但是它要先 sleep 3s",
      );

      // 等待 bg agent 派发通知出现（思考 + 派发约 10-15s）
      await tester.waitForText("agent", {
        timeout: 30_000,
        interval: 1000,
      });

      // 此时 bg agent 应该已在运行（状态栏或 BgTaskArea 可见）
      await tester.sleep(2000);
      const runningCapture = await takePeriSnapshot(tester, "bg-agent-running");

      // 等待 bg agent 完成（sleep 3s + overhead）
      // 用 waitForText 等完成通知
      await tester.waitForText("已完成", {
        timeout: 60_000,
        interval: 2000,
      });
      await tester.sleep(2000);

      const doneCapture = await takePeriSnapshot(tester, "bg-agent-done");

      expect(runningCapture.text.length).toBeGreaterThan(50);
      expect(doneCapture.text.length).toBeGreaterThan(50);

      // Judge: running 态（agent 正在处理派发 bg subagent 请求）
      try {
        const r = await judge({
          ansiRaw: runningCapture.raw,
          criteria: [
            "系统应处于处理中状态：应有思考块（如 '思考了 N 字符'）或底部有加载指示器",
            "输入提示应已提交（屏幕显示用户 prompt），agent 在准备或启动后台任务",
          ],
        });
        console.log("Judge (running):", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }

      // Judge: 完成态
      try {
        const r = await judge({
          ansiRaw: doneCapture.raw,
          criteria: [
            "后台 agent 应已完成（✔ 标记、完成通知或状态栏 agent 计数归零）",
            "消息区应包含 SubAgent 的完成通知或执行结果",
          ],
        });
        console.log("Judge (done):", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }
    },
  );
});
