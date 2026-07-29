/**
 * 回归测试: /clear 命令清空消息区后旧消息不再恢复
 *
 * 复现 spec/issues/2026-07-07-slash-clear-messages-reappear-after-1s.md
 * 症状: /clear 后消息区短暂清空，约 1s 后旧对话全部恢复
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("scenarios: /clear 命令", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "/clear 后旧对话消息不再恢复",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      const base = await tester.getScreenText();

      // 第一轮: 产生一条有特征的回复
      await sendPrompt(tester, "请用一句中文回复: 第一轮测试消息");
      await waitForStableScreen(tester, 120_000, base);

      // 第二轮: 再产生一条回复，积累更多消息
      const round2Base = await tester.getScreenText();
      await sendPrompt(tester, "请用一句中文回复: 第二轮测试消息");
      await waitForStableScreen(tester, 120_000, round2Base);

      // 记录 clear 前的屏幕，确认有历史消息
      await tester.sleep(500);
      const beforeClear = await takePeriSnapshot(tester, "clear-before");

      expect(beforeClear.text).toContain("第一轮测试消息");
      expect(beforeClear.text).toContain("第二轮测试消息");

      // 先按 Esc 确保输入区处于正常模式
      await tester.sendKey("Escape");
      await tester.sleep(200);

      // 逐字符输入 /clear（会触发 slash 弹窗）
      for (const char of "/clear") {
        await tester.sendText(char);
        await tester.sleep(80);
      }
      // 第一次 Enter：弹窗选中命令、替换文本、关闭弹窗
      await tester.sendKey("Enter");
      await tester.sleep(300);
      // 第二次 Enter：提交输入区的文本（真正执行 /clear）
      await tester.sendKey("Enter");
      await tester.sleep(500);

      // 等待 5 秒——issue 描述旧消息约 1s 后恢复
      await tester.sleep(5000);

      // 再抓一帧确认稳定性
      await tester.sleep(1000);
      const afterClear = await takePeriSnapshot(tester, "clear-after");

      // 基本断言
      expect(afterClear.text.length).toBeGreaterThan(0);

      // LLM judge
      const result = await judge({
        ansiRaw: afterClear.raw,
        criteria: [
          "消息区域不应出现'第一轮测试消息'或'第二轮测试消息'这两条对话内容",
          "消息区应处于清空状态（空白或仅显示欢迎页/logo），不应有历史对话消息",
          "界面底部输入框应仍然可见可用",
        ],
      });
      console.log("Judge (/clear):", JSON.stringify(result, null, 2));
      expect(result.pass).toBe(true);
    },
  );
});
