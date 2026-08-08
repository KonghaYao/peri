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
      // 注意：tmux viewport ≈40 行，超过 2 轮会滚出屏幕
      const base = await tester.getScreenText();
      await sendPrompt(tester, "用中文简短回复: 今天天气不错");
      await waitForStableScreen(tester, 120_000, base);

      const r2 = await tester.getScreenText();
      await sendPrompt(tester, "用中文简短回复: 后天转晴");
      await waitForStableScreen(tester, 120_000, r2);

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

      // 等待 compact 处理完成：SystemNote（"*压缩完成"）注入后再等待屏幕稳定。
      // 历史教训（2026-08-06 第二轮失败）：原实现固定 sleep 3.5s 后截图，
      // 此时 compact 仍在处理中（spinner "学以致用" 转动、消息区未折叠、
      // 无完成提示行），Judge 将处理中的稀疏布局（大量留白）误判为渲染
      // 异常（失败录制 compact-after.txt 与 before 布局同构，证实为等待
      // 时机问题而非渲染缺陷）。
      // 完成提示文案随 locale 变化：zh "压缩完成" / en "compaction completed"
      // （e2e 环境 LANG=C.UTF-8，fluent 回退英文），两者都接受。
      await tester.waitFor(
        (screen) => screen.includes("压缩完成") || screen.includes("compaction completed"),
        { timeout: 120_000, interval: 1000, message: "/compact 完成提示（压缩完成/compaction completed）未出现" },
      );
      await waitForStableScreen(tester, 60_000);

      // 抓取 compact 后的屏幕
      const afterCompact = await takePeriSnapshot(tester, "compact-after");

      expect(afterCompact.text.length).toBeGreaterThan(0);

      // LLM judge —— 正向可观察断言。历史教训：负向模糊断言
      // "消息区域不应出现渲染异常" 会让 Judge 把正常的消息区留白
      // （内容不满屏）误判为渲染残留，导致 flaky（2026-08-06 两轮运行
      // 一轮通过一轮失败，录制布局与通过用例一致）。改为正向断言。
      const result = await judge({
        ansiRaw: afterCompact.raw,
        criteria: [
          "屏幕消息区应显示上下文压缩完成的卡片或提示行（如包含 '温故知新'、'压缩'、'完成' 或 token 数信息，如 '↓ 15k tokens'）",
          "界面底部输入框应仍然可见可用（包含 '❯' 提示符）",
          "状态栏应显示上下文消耗百分比（格式如 'NN% NNNk'），百分比数值应合理（>0% 且 <=100%）",
        ],
      });
      console.log("Judge (/compact):", JSON.stringify(result, null, 2));
      expect(result.pass).toBe(true);
    },
  );
});
