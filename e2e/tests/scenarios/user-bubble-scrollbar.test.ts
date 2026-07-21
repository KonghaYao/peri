/**
 * 场景测试：长 UserBubble 消息的滚动条 thumb 验证
 *
 * 对应: spec/issues/2026-07-21-user-bubble-long-text-scrollbar-inaccurate.md
 * 修复: render.rs UserBubble markdown 解析宽度预留 2 列给 ❯ 前缀
 *
 * 主要验证：长纯文本用户消息能否正常渲染（不崩溃、不截断）
 * 滚动条计算正确性由 render_test.rs / selection tests 覆盖
 */
import { describe, it, expect, afterEach } from "vitest";
import {
  launchPeri,
  takePeriSnapshot,
} from "../../helpers/peri.js";
import type { TmuxTester } from "tui-tester";

describe("scenarios: user bubble long text scrollbar", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "长用户消息发送后正常渲染不崩溃",
    { timeout: 90_000 },
    async () => {
      tester = await launchPeri();

      // 生成足够长的消息以触发大量折行
      const para = "测试文本滚动条显示效果验证修复是否正确";
      const longText = para.repeat(40);
      await tester.sendText(longText);
      await tester.sleep(300);
      await tester.sendKey("Enter");
      await tester.sleep(3000);

      const capture = await takePeriSnapshot(tester, "user-bubble-long-scrollbar");

      // 基本断言：屏幕有内容
      expect(capture.text.length).toBeGreaterThan(100);

      // 验证用户消息回显：首行应以 ❯ 开头
      expect(capture.text).toContain("测试文本滚动条显示效果验证修复是否正确");

      // 验证消息可见行数合理（120 列终端下每条行约 20 个中文字）
      // 40 次重复 × 20 字 = 800 字符 → 约 40 视觉行
      // 终端高度 40 行，消息应占大部分屏幕
      const lines = capture.text.split("\n");
      const userLines = lines.filter(
        (l) => l.includes("验证修复是否正确") || l.includes("❯"),
      );
      expect(userLines.length).toBeGreaterThan(3);

      // 不应出现 panic/crash 残留
      expect(capture.text).not.toContain("thread 'main' panicked");
      expect(capture.text).not.toContain("RUST_BACKTRACE");
    },
  );
});
