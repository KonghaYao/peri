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
    { timeout: 480_000 },
    async () => {
      tester = await launchPeri();

      await sendPrompt(
        tester,
        "请严格使用 Bash 工具执行 sleep 10，不要只解释命令。",
      );

      // 等待真实的运行中工具行。不能匹配裸 "Bash"：用户 prompt 本身含该词，
      // 会在工具尚未派发时假绿并过早抓屏。
      await tester.waitFor(
        (screen) =>
          /(?:^|\n)\s*[\u2800-\u28ff]\s+(?:Bash|Shell)\b[^\n]*sleep 10/m.test(
            screen,
          ),
        {
          timeout: 90_000,
          interval: 500,
          message: "等待 Bash sleep 10 进入运行态",
        },
      );
      // sleep 10 留出确定的运行窗口，约 3s 时抓拍时长。
      await tester.sleep(3000);

      const runningCapture = await takePeriSnapshot(
        tester,
        "bash-running-duration",
      );

      // 等待同一工具完成以及主 turn footer 出现，再抓完成态。
      await tester.waitFor(
        (screen) =>
          /✓\s+(?:Bash|Shell)\b[^\n]*sleep 10/.test(screen) &&
          /(?:Brewed for|处理耗时)/.test(screen),
        {
          timeout: 180_000,
          interval: 500,
          message: "等待 Bash sleep 10 与主 turn 完成",
        },
      );

      const doneCapture = await takePeriSnapshot(tester, "bash-done");

      expect(runningCapture.text.length).toBeGreaterThan(50);
      expect(doneCapture.text.length).toBeGreaterThan(50);

      // Judge: 运行中
      const r = await judge({
        ansiRaw: runningCapture.raw,
        criteria: [
          "屏幕上应出现 Bash 或 Shell 工具调用的痕迹",
          "Bash 工具应处于运行中状态（可能有时长指示器如 'Running' 或时长数字）",
        ],
      });
      console.log("Judge (running):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 完成阶段
      const r2 = await judge({
        ansiRaw: doneCapture.raw,
        criteria: [
          "Bash 工具应已完成——不应再显示 'Running' 状态",
          "agent 应已收到工具结果并给出后续回复或总结",
        ],
      });
      console.log("Judge (done):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);
    },
  );
});
