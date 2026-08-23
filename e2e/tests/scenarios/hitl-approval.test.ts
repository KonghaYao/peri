/**
 * 场景测试: HITL 审批弹窗
 *
 * 验证 -a 模式下敏感工具调用触发审批弹窗：
 * - 弹窗标题 "审批请求" / "Approval Required"
 * - 工具信息（名称、参数）可读
 * - Enter 批准后工具继续执行
 * - 工具执行结果正常显示
 *
 * 注意：此测试需要 -a 模式启动 peri，审批交互涉及 HITL 中间件。
 * 审批超时默认 120s，测试需在超时前完成交互。
 */
import { describe, it, expect, afterEach } from "vitest";
import {
  launchPeriHITL,
  sendPrompt,
  takePeriSnapshot,
  waitForStableScreen,
} from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("scenario: hitl approval", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "HITL 审批弹窗显示并批准后工具执行",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeriHITL();

      // 触发需要审批的工具（Bash 在默认审批模式下需要审批）
      await sendPrompt(
        tester,
        "请用 Shell 执行命令 echo 'hitl_test_success'",
      );

      // 等待 HITL 审批弹窗出现
      await tester.waitForText("审批请求", {
        timeout: 60_000,
        interval: 1000,
      });
      await tester.sleep(1000);

      const popupCapture = await takePeriSnapshot(tester, "hitl-popup");

      // 批准：按 Enter
      await tester.sendKey("enter");
      await tester.sleep(500);

      // 等待审批响应写回 inline interaction；该文本不会命中 prompt 回显。
      await tester.waitForText("Allowed once", {
        timeout: 60_000,
        interval: 500,
      });
      // 等待 marker 再次出现。第一次来自用户 prompt；第二次只能来自真实工具结果
      // 或消费该结果后的 agent 回复，因此不会把 prompt 回显误判为成功。
      await tester.waitFor(
        (screen) => (screen.match(/hitl_test_success/g) ?? []).length >= 2,
        {
          timeout: 60_000,
          interval: 500,
          message: "等待 Bash 工具结果被 TUI/agent 消费",
        },
      );
      // 继续等待工具完成和 agent 最终回复稳定，避免在 reasoning 阶段抓屏。
      await waitForStableScreen(tester, 120_000, popupCapture.text);

      const doneCapture = await takePeriSnapshot(tester, "hitl-done");

      expect(popupCapture.text.length).toBeGreaterThan(50);
      expect((doneCapture.text.match(/hitl_test_success/g) ?? []).length).toBeGreaterThanOrEqual(2);

      // Judge: 弹窗阶段
      const r = await judge({
        ansiRaw: popupCapture.raw,
        criteria: [
          "应出现审批弹窗（如 '审批请求' 或 'Approval Required' 标题）",
          "弹窗中应显示待审批工具的信息（如 'Bash' 或 'Shell'）",
          "弹窗中应有操作提示（如 'Enter: 批准' 或 'Enter: approve'）",
        ],
      });
      console.log("Judge (popup):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 完成阶段
      const r2 = await judge({
        ansiRaw: doneCapture.raw,
        criteria: [
          "审批弹窗应已关闭（不再显示 '审批请求' 标题）",
          "审批结果应已回写为 'Allowed once'，且 Bash/Shell 工具卡呈成功完成态",
          "消息区应显示真实命令输出 'hitl_test_success'，agent 应引用该结果完成最终回复",
        ],
      });
      console.log("Judge (done):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);
    },
  );
});
