/**
 * 场景测试: Plugin 面板行为（Tab 切换、Marketplaces 列表、详情、Esc 关闭）
 *
 * 验证 /plugin 命令打开插件面板后，四个 Tab（已安装 / 探索 / 市场 / 错误）
 * 的导航与交互行为符合预期。不依赖网络请求（不测试 marketplace refresh 和安装）。
 *
 * 注意：本测试依赖配置中的 locale 设置（当前为 zh-CN），Tab 文本使用中文。
 */
import { describe, it, expect, afterEach } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("panels: plugin", () => {
  let tester: TmuxTester;
  let testHome: string;

  function createChineseHome(): string {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "peri-e2e-plugin-home-"));
    fs.mkdirSync(path.join(home, ".peri"), { recursive: true });
    fs.writeFileSync(
      path.join(home, ".peri", "settings.json"),
      JSON.stringify({ config: { language: "zh-CN" } }),
    );
    const pluginsDir = path.join(home, ".claude", "plugins");
    const marketplaceDir = path.join(pluginsDir, "marketplaces", "claude-plugins-official");
    fs.mkdirSync(marketplaceDir, { recursive: true });
    fs.writeFileSync(
      path.join(pluginsDir, "known_marketplaces.json"),
      JSON.stringify({
        "claude-plugins-official": {
          source: { source: "github", repo: "anthropics/claude-plugins-official" },
          installLocation: marketplaceDir,
          autoUpdate: true,
          lastUpdated: "2026-08-16T00:00:00Z",
        },
      }),
    );
    fs.writeFileSync(
      path.join(marketplaceDir, "marketplace.json"),
      JSON.stringify({
        name: "claude-plugins-official",
        plugins: [{ name: "example-plugin", version: "1.0.0", description: "Fixture plugin" }],
      }),
    );
    return home;
  }

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
    if (testHome) {
      fs.rmSync(testHome, { recursive: true, force: true });
      testHome = "";
    }
  });

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "/plugin 打开面板，Tab 切换和 Marketplaces 列表正常显示",
    { timeout: 180_000 },
    async () => {
      testHome = createChineseHome();
      tester = await launchPeri({ env: { HOME: testHome } });

      // ── 阶段 1：/plugin 打开插件面板 ──
      await sendPrompt(tester, "/plugin");

      // 等待插件面板出现（至少看到 Tab 栏的 "已安装" 标签）
      await tester.waitForText("已安装", {
        timeout: 10_000,
        interval: 500,
      });

      const panelOpenCapture = await takePeriSnapshot(tester, "plugin-panel-open");

      // ── 阶段 2：Tab 切换 Right → 探索, Right → 市场 ──
      await tester.sendKey("right");
      await tester.sleep(300);
      await tester.sendKey("right");
      await tester.sleep(300);

      const marketplacesCapture = await takePeriSnapshot(tester, "plugin-marketplaces-tab");

      // ── 阶段 3：在 Marketplaces tab 中上下导航 ──
      await tester.sendKey("down");
      await tester.sleep(200);
      await tester.sendKey("up");
      await tester.sleep(200);

      const marketplacesNavCapture = await takePeriSnapshot(tester, "plugin-marketplaces-nav");

      // ── 阶段 4：切到 Errors tab ──
      await tester.sendKey("right");
      await tester.sleep(300);

      const errorsTabCapture = await takePeriSnapshot(tester, "plugin-errors-tab");

      // ── 阶段 5：Esc 关闭面板 ──
      await tester.sendKey("escape");
      await tester.sleep(500);

      const panelClosedCapture = await takePeriSnapshot(tester, "plugin-panel-closed");

      // ── 基本断言 ──
      expect(panelOpenCapture.text).toContain("已安装");
      expect(marketplacesCapture.text.length).toBeGreaterThan(30);

      // ── LLM judge：面板打开阶段 ──
      const openResult = await judge({
        ansiRaw: panelOpenCapture.raw,
        criteria: [
          "屏幕中应显示 Plugin 面板，包含四个 Tab 标签：已安装、探索、市场、错误",
        ],
      });
      console.log("Judge (panel-open):", JSON.stringify(openResult, null, 2));
      expect(openResult.pass).toBe(true);

      // ── LLM judge：Marketplaces tab ──
      const mpResult = await judge({
        ansiRaw: marketplacesCapture.raw,
        criteria: [
          "市场 Tab 应为当前激活的 Tab（高亮/反色样式）",
          "内容区域应列出至少一个 marketplace 来源（如 claude-plugins-official），含来源类型标签（如 GitHub/Git/URL）和插件数量",
        ],
      });
      console.log("Judge (marketplaces):", JSON.stringify(mpResult, null, 2));
      expect(mpResult.pass).toBe(true);

      // ── 确定性断言：Errors tab（错误列表为空，替代原 judge 调用）──
      expect(errorsTabCapture.text).toContain("加载错误 (0)");

      // ── 确定性断言：面板已关闭，回到主聊天界面（替代原 judge 调用）──
      expect(panelClosedCapture.text).not.toContain("已安装");
      expect(panelClosedCapture.text).not.toContain("Marketplaces");
      expect(panelClosedCapture.text).toContain("/model  /agents");
    },
  );
});
