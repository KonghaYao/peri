/**
 * 工具卡片场景: 工具输出截断
 *
 * 验证 Read 大文件时 canonical tool result 的截断语义贯穿到 TUI：
 * - Read 成功完成，折叠态头行显示可见行数
 * - canonical result 的截断状态在 tool card 摘要中可见
 * - agent 能基于截断输出及 continuation hint 继续工作
 */
import { describe, it, expect, afterEach } from "vitest";
import {
  launchPeri,
  sendPrompt,
  takePeriSnapshot,
  waitForStableScreen,
} from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("tool-card: output truncation", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "Read 大文件时输出被截断",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // Cargo.lock 通常很大，输出会被截断
      const baseScreen = await tester.getScreenText();
      await sendPrompt(
        tester,
        "请严格只调用一次 Read 工具，读取当前工作目录下的 Cargo.lock 文件（file_path 必须是 Cargo.lock，不要读取 Cargo.toml 或其他文件），然后根据读取结果继续回答。",
      );

      // 等待目标 Read tool card 的独有完成态组合，避免命中 reasoning 或最终回复中的
      // 通用 "truncated" 文本后过早抓屏。
      await tester.waitForPattern(/✓\s+Read Cargo\.lock\s+—\s+\d+ lines · truncated/, {
        timeout: 60_000,
        interval: 500,
        message: "等待 Read Cargo.lock 截断工具卡完成态",
      });

      const readCapture = await takePeriSnapshot(
        tester,
        "tool-truncation-read",
      );

      // 等待 agent 基于真实工具结果完成回答。
      await waitForStableScreen(tester, 120_000, baseScreen);
      const afterCapture = await takePeriSnapshot(
        tester,
        "tool-truncation-after",
      );

      expect(readCapture.text).toMatch(/✓\s+Read Cargo\.lock\s+—\s+\d+ lines · truncated/);
      // 最终分析较长时，已验证过的工具头行可以正常滚出视口；完成态只验证
      // agent 基于该 canonical result 给出了后续分析。

      // Judge: Read 阶段只评估真实可观察链路，不从行数反推截断。
      const r = await judge({
        ansiRaw: readCapture.raw,
        criteria: [
          "屏幕上应出现成功完成的 Read Cargo.lock 工具卡（✓ 状态）",
          "Read 工具头行应同时显示可见行数和明确的 'truncated' 状态",
        ],
      });
      console.log("Judge (read):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 完成阶段
      const r2 = await judge({
        ansiRaw: afterCapture.raw,
        criteria: [
          "agent 应基于读取的内容给出分析或总结",
          "agent 能基于可见内容给出有价值的分析，即使提到了截断也不影响分析质量",
        ],
      });
      console.log("Judge (after):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);
    },
  );
});
