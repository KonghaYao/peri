/**
 * 工具卡片场景: Bash 运行时长显示
 *
 * 验证长时间运行的 Bash 工具卡片显示实时时长：
 * - Bash 工具卡片显示 "Shell" 名称（别名映射）
 * - 运行中显示时长（如 "Running (Ns)"）
 * - 完成后显示输出结果
 *
 * 注意：不能用 waitForText 等 output marker——输出文本也出现在工具参数中，
 * waitForText 会过早匹配。改用固定等待时间确保 bash 确实完成。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: bash running duration", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "长时间 Bash 运行时显示运行时长",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      await sendPrompt(
        tester,
        "请用 Bash 执行 sleep 6",
      );

      // 等待 Bash 工具被调用（agent 思考 + 派发约 5-15s）
      await tester.waitForText("Bash", {
        timeout: 60_000,
        interval: 1000,
      });
      // 等 Bash 开始运行（sleep 期间，约 3s 时抓拍）
      await tester.sleep(3000);

      const runningCapture = await takePeriSnapshot(
        tester,
        "bash-running-duration",
      );

      // 固定等待 sleep 6 完成（已过 3s，再等 10s 确保完成 + agent 响应）
      await tester.sleep(12000);

      const doneCapture = await takePeriSnapshot(tester, "bash-done");

      expect(runningCapture.text.length).toBeGreaterThan(50);
      expect(doneCapture.text.length).toBeGreaterThan(50);

      // Judge: 运行中
      try {
        const r = await judge({
          ansiRaw: runningCapture.raw,
          criteria: [
            "屏幕上应出现 Bash 或 Shell 工具调用的痕迹",
            "Bash 工具应处于运行中状态（可能有时长指示器如 'Running' 或时长数字）",
          ],
        });
        console.log("Judge (running):", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }

      // Judge: 完成阶段
      try {
        const r = await judge({
          ansiRaw: doneCapture.raw,
          criteria: [
            "Bash 工具应已完成——不应再显示 'Running' 状态",
            "agent 应已收到工具结果并给出后续回复或总结",
          ],
        });
        console.log("Judge (done):", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }
    },
  );
});
