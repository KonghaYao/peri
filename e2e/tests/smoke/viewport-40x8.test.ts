import { buildPeriForE2e } from "../../helpers/build.js";
/**
 * 冒烟场景: 40×8 极小视口（§11 响应式降级，Slice 1）
 *
 * 高度 < 12：隐藏 session title 与 key hints；< 8：composer ≤2 行。
 * 40×8 下 transcript 至少保留 3 行、composer 可操作、无 panic。
 *
 * [Slice 5] 新增场景（§15 golden scene：40×8 viewport）。
 * 依赖 Slice 1 的 `layout_plan` 高度断点（status 2 行 + composer 3 行
 * → transcript = 3 行）。
 *
 * 等待策略（Slice 5 修复记录）：40×8 下 3 行视口只显示最新内容（footer
 * 处理耗时行），user bubble "hi" 与回答文本在视口上方（顶部 ▲ 指示）——
 * `waitForText("hi")` 必然超时。改为等待 turn 完成标志（footer 处理耗时）。
 *
 * 驱动方式（L0 确定性改造）：隔离 HOME + 本地假 model server 重放
 * （`helpers/replay-model.ts`；先例 workspace-mcp-resources-tui），
 * 不依赖真实模型/凭据/外部网络。
 */
import { describe, it, expect, afterEach, beforeAll } from "vitest";
import { rm } from "node:fs/promises";
import { sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import {
  launchReplayTui,
  makeReplayHome,
  replayDebug,
  startReplayModelServer,
  type ReplayServer,
} from "../../helpers/replay-model.js";
import type { TmuxTester } from "tui-tester";

describe("smoke: 40x8 minimal viewport", () => {
  let tester: TmuxTester;
  let replay: ReplayServer;
  let home: string;

  beforeAll(async () => {
    // 控制面脚本不构建 binary；本用例必须跑当前源码。
    await buildPeriForE2e();
  }, 610_000);

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
    if (replay) await replay.close();
    if (home) await rm(home, { recursive: true, force: true });
  });

  it(
    "40×8 下正常提问回答（transcript 保留、无 panic）",
    { timeout: 300_000 },
    async () => {
      replay = await startReplayModelServer([{ prompt: "hi", text: "REPLAY-40X8-REPLY" }]);
      home = await makeReplayHome(replay.port);
      tester = await launchReplayTui({ home, size: { cols: 40, rows: 8 } });

      await sendPrompt(tester, "hi");

      // 40×8 下 user bubble 被滚出 3 行视口——等待可见的中英文 footer 任一项。
      await tester.waitFor(
        (screen) => screen.includes("处理耗时") || screen.includes("Brewed for"),
        { timeout: 180_000, interval: 1000, message: "等待 turn footer 超时" },
      );
      await tester.sleep(1500);

      const capture = await takePeriSnapshot(tester, "smoke-40x8");
      expect(capture.text.length).toBeGreaterThan(10);

      // footer（turn 完成标志）必须可见——硬断言（40×8 下 3 行视口只显示
      // 最新内容：滚动指示 + footer；回答文本在视口上方是 §11 设计行为）
      const hasFooter =
        capture.text.includes("处理耗时") || capture.text.includes("Brewed for");
      expect(hasFooter, `footer 处理耗时可见：${capture.text.slice(-300)}`).toBe(
        true,
      );

      // composer 的提示符必须仍可见，确认 turn 完成后输入区未被挤出视口。
      expect(capture.text).toContain("❯");

      // 重放契约：剧本必须全部命中（未命中会回固定文本，掩盖真实链路偏航）。
      expect(replay.misses(), `剧本全部命中：${replayDebug(replay)}`).toBe(0);
    },
  );
});
