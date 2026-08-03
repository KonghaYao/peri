/**
 * 场景测试: Plugin 面板卸载操作不冻结 UI
 *
 * 回归保护：2026-08-02-plugin-panel-uninstall-enter-freeze.md
 * 详情页选中 Uninstall 后按 Enter 进确认模式时，事件线程曾因
 * RwLock 同线程 read→write 重入死锁导致 UI 永久冻结。
 *
 * 本测试只进入确认模式（不真正卸载），用"确认提示出现 + 后续按键
 * 屏幕仍变化"断言 UI 未被冻结。不依赖具体插件内容，无副作用。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import type { TmuxTester } from "tui-tester";

describe("panels: plugin uninstall no-freeze", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "详情页 Enter 卸载进入确认模式，UI 保持响应",
    { timeout: 90_000 },
    async () => {
      tester = await launchPeri();

      // ── 打开 plugin 面板 ──
      await sendPrompt(tester, "/plugin");
      await tester.waitForText("已安装", { timeout: 10_000, interval: 500 });

      // ── Enter 进入第一个插件详情（面板有已加载插件时才有效）──
      await tester.sendKey("Enter");
      await tester.sleep(800);
      const detailText = (await tester.captureScreen()).text;
      // 若无插件可进详情，跳过后续断言（环境无关性）
      if (!detailText.includes("操作")) {
        console.log("SKIP: 无已安装插件，无法进入详情");
        return;
      }

      // ── down 选中 Uninstall action ──
      await tester.sendKey("down");
      await tester.sleep(300);

      // ── Enter → 应进入确认模式（footer 出现 "Enter: 确认  Esc: 取消"）──
      await tester.sendKey("Enter");
      await tester.sleep(800);
      const confirmCap = await takePeriSnapshot(tester, "plugin-uninstall-confirm-mode");
      expect(confirmCap.text).toContain("确认");

      // ── UI 响应性：确认模式下按 down（应关闭确认弹窗，屏幕变化）──
      const before = confirmCap.text;
      await tester.sendKey("down");
      await tester.sleep(600);
      const after = await tester.captureScreen();
      expect(after.text).not.toEqual(before);
    },
  );
});
