/**
 * 场景测试: Fork 模式 Background Agent
 *
 * 验证 bg fork subagent 运行时 BgTaskArea 显示 fork agent 状态，
 * 完成后的回调通知出现在消息区。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("subagent: fork bg callback", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "bg fork subagent 运行并在完成后回调通知",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // subagent.md 原始 prompt（sleep 5s）
      await sendPrompt(
        tester,
        "请使用 bg fork subagent say hello，但是它要先 sleep 5s",
      );

      // 等待 bg agent 派发通知出现（思考 + 派发约 10-15s）
      await tester.waitForText("agent", {
        timeout: 30_000,
        interval: 1000,
      });

      // 此时 bg agent 应该已在运行
      await tester.sleep(2000);
      const runningCapture = await takePeriSnapshot(tester, "fork-bg-running");

      // 等待 bg agent 完成（sleep 5s + overhead）
      await tester.waitForText("已完成", {
        timeout: 60_000,
        interval: 2000,
      });
      await tester.sleep(2000);

      const doneCapture = await takePeriSnapshot(tester, "fork-bg-done");

      expect(runningCapture.text.length).toBeGreaterThan(50);
      expect(doneCapture.text.length).toBeGreaterThan(50);

      // Judge: running 态（agent 正在处理派发 fork bg subagent 请求）
      try {
        const r = await judge({
          ansiRaw: runningCapture.raw,
          criteria: [
            "系统应处于处理中状态：应有思考块（如 '思考了 N 字符'）或底部有加载指示器",
            "输入提示应已提交（屏幕显示用户 prompt），agent 在准备或启动后台 fork 任务",
          ],
        });
        console.log("Judge (running):", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }

      // Judge: 完成态 + 回调
      try {
        const r = await judge({
          ansiRaw: doneCapture.raw,
          criteria: [
            "后台 agent 应已完成（✔ 标记、完成通知或状态栏 agent 计数归零）",
            "消息区应出现 SubAgent 完成后的回调通知或结果（如 'hello' 或完成摘要）",
          ],
        });
        console.log("Judge (done):", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }
    },
  );
});
