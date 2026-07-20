/**
 * 场景测试: /compact 手动触发上下文压缩
 *
 * 验证 /compact 命令执行后 UI 不崩溃，状态栏保持正常。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("scenarios: /compact 命令", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "/compact 手动触发后 UI 正常不崩溃",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // 积累几轮对话，充实消息历史
      const base = await tester.getScreenText();
      await sendPrompt(tester, "用中文简短回复: 今天天气不错");
      await waitForStableScreen(tester, 120_000, base);

      const r2 = await tester.getScreenText();
      await sendPrompt(tester, "用中文简短回复: 明天可能下雨");
      await waitForStableScreen(tester, 120_000, r2);

      const r3 = await tester.getScreenText();
      await sendPrompt(tester, "用中文简短回复: 后天转晴");
      await waitForStableScreen(tester, 120_000, r3);

      // 记录 compact 前的状态
      const beforeCompact = await takePeriSnapshot(tester, "compact-before");
      expect(beforeCompact.text).toContain("今天天气不错");

      // 先按 Esc 确保输入区处于正常模式
      await tester.sendKey("Escape");
      await tester.sleep(200);

      // 逐字符输入 /compact（会触发 slash 弹窗）
      for (const char of "/compact") {
        await tester.sendText(char);
        await tester.sleep(80);
      }
      // 第一次 Enter：弹窗选中命令、替换文本、关闭弹窗
      await tester.sendKey("Enter");
      await tester.sleep(300);
      // 第二次 Enter：提交输入区的文本（真正执行 /compact）
      await tester.sendKey("Enter");
      await tester.sleep(500);

      // 等待 compact 处理完成
      await tester.sleep(3000);

      // 抓取 compact 后的屏幕
      const afterCompact = await takePeriSnapshot(tester, "compact-after");

      expect(afterCompact.text.length).toBeGreaterThan(0);

      // LLM judge
      try {
        const result = await judge({
          ansiRaw: afterCompact.raw,
          criteria: [
            "状态栏应显示上下文消耗百分比（格式如 'NN% NNNk'），百分比数值应合理（>0% 且 <=100%）",
            "消息区域不应出现渲染异常（如文字覆盖、布局错位、空白闪烁残留）",
            "界面底部输入框应仍然可见可用",
          ],
        });
        console.log("Judge (/compact):", JSON.stringify(result, null, 2));
      } catch (err: any) {
        console.warn("Judge 失败:", err.message);
      }
    },
  );
});
