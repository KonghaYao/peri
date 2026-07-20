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

      // 等待工具执行完成（echo 很快，但要等 LLM 响应）
      await tester.waitForText("hitl_test_success", {
        timeout: 60_000,
        interval: 2000,
      });
      await tester.sleep(2000);

      const doneCapture = await takePeriSnapshot(tester, "hitl-done");

      expect(popupCapture.text.length).toBeGreaterThan(50);
      expect(doneCapture.text.length).toBeGreaterThan(50);

      // Judge: 弹窗阶段
      try {
        const r = await judge({
          ansiRaw: popupCapture.raw,
          criteria: [
            "应出现审批弹窗（如 '审批请求' 或 'Approval Required' 标题）",
            "弹窗中应显示待审批工具的信息（如 'Bash' 或 'Shell'）",
            "弹窗中应有操作提示（如 'Enter: 批准' 或 'Enter: approve'）",
          ],
        });
        console.log("Judge (popup):", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }

      // Judge: 完成阶段
      try {
        const r = await judge({
          ansiRaw: doneCapture.raw,
          criteria: [
            "审批弹窗应已关闭（不再显示 '审批请求' 标题）",
            "Bash 工具应已执行完毕（消息区出现 'hitl_test_success' 输出）",
            "agent 的回复应引用工具执行结果",
          ],
        });
        console.log("Judge (done):", JSON.stringify(r, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }
    },
  );
});
