/**
 * 场景测试: Slash 命令 + 模型切换
 *
 * 验证 /model 斜杠命令打开模型面板，用户可通过键盘选择切换模型，
 * 状态栏中的 provider/model 随之更新。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("panels: model switch", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "/model 打开面板并切换模型，状态栏更新",
    { timeout: 120_000 },
    async () => {
      tester = await launchPeri();

      // 阶段 1：通过 /model 命令打开模型面板
      await sendPrompt(tester, "/model");

      await tester.waitForText("Model", {
        timeout: 10_000,
        interval: 500,
      });

      const panelCapture = await takePeriSnapshot(tester, "model-panel-open");

      // 阶段 2：在面板中选择不同模型（Down + Enter）
      await tester.sendKey("down");
      await tester.sleep(200);
      await tester.sendKey("Enter");
      await tester.sleep(1000);

      const capture = await takePeriSnapshot(tester, "model-switch-done");

      // 基本断言
      expect(panelCapture.text).toContain("Model");
      expect(capture.text.length).toBeGreaterThan(50);

      // LLM judge: 面板阶段
      const panelResult = await judge({
        ansiRaw: panelCapture.raw,
        criteria: [
          "屏幕中应有 Model 面板，包含可选的模型别名（如 Opus、Sonnet、Haiku）",
          "面板中应有指示当前选中项的标记（如 > 光标或 ✔ 选中标识）",
        ],
      });
      console.log("Judge (panel):", JSON.stringify(panelResult, null, 2));
      expect(panelResult.pass).toBe(true);

      // LLM judge: 切换后状态栏验证
      const doneResult = await judge({
        ansiRaw: capture.raw,
        criteria: [
          "Model 面板应已关闭，屏幕底部状态栏应显示 provider/model 信息（如 'openai/xxx' 或 'anthropic/xxx' 格式）",
          "状态栏的 model 部分应与切换后的模型一致，不应仍是默认值",
        ],
      });
      console.log("Judge (done):", JSON.stringify(doneResult, null, 2));
      expect(doneResult.pass).toBe(true);
    },
  );
});
