/**
 * 场景测试: SubAgent 内部工具调用卡片可见性
 *
 * 回归测试 for issue: 2026-07-18-subagent-tool-cards-regression-empty
 *
 * 验证当主 agent 调用同步 SubAgent 时，Agent 卡片内部的具体工具调用卡片
 * （如 ● Grep、● Read）确实可见——而非仅 Agent 卡片容器外壳、内部为空壳。
 *
 * 与 agent-output-position.test.ts 的差异：
 * - agent-output-position 验证「子工具在 Agent 卡片下方」，侧重位置
 * - 本测试验证「子工具卡片具体内容可见」，侧重非空
 *
 * 历史：2026-07-13 曾出现相同症状（or_insert_with 复用关闭的 channel），
 * 修复后回归。本次根因为 event_sink.rs 中 caps.source_agent_id gating
 * 阻止了 _peri.sourceAgentId 注入。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("subagent: internal tool cards visibility (regression)", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "SubAgent 内部工具调用卡片（Grep/Read 等）应可见且非空壳",
    { timeout: 420_000 },
    async () => {
      tester = await launchPeri();

      // 记录提交前的屏幕（用于 waitForStableScreen 基准）
      const base = await tester.getScreenText();

      // 使用 explorer subagent 搜索代码——保证会调用 Grep/Read 等工具
      await sendPrompt(
        tester,
        "请使用同步 explorer subagent 搜索 peri-tui/src/kit 目录下所有包含 'subagent' 关键词的文件和函数",
      );

      // 等待 Agent 工具卡出现
      await tester.waitForText("Agent", {
        timeout: 60_000,
        interval: 1000,
      });

      // 让 explorer 跑起来执行工具
      await tester.sleep(10000);

      const runningCapture = await takePeriSnapshot(
        tester,
        "subagent-internal-toolcards-running",
      );

      // 等待 SubAgent 完成：等屏幕稳定（subagent 工具卡片 + 主 agent 总结全部结束）。
      // 固定 sleep 不可靠——thorough explorer 运行时长不确定。
      await waitForStableScreen(tester, 180_000, base);

      const doneCapture = await takePeriSnapshot(
        tester,
        "subagent-internal-toolcards-done",
      );

      expect(runningCapture.text.length).toBeGreaterThan(50);
      expect(doneCapture.text.length).toBeGreaterThan(50);

      // Judge: 运行中 —— 内部工具调用卡片可见
      const r = await judge({
        ansiRaw: runningCapture.raw,
        criteria: [
          // 核心断言：内部工具卡片非空壳
          "消息区中应出现 SubAgent 内部工具调用的具体卡片（如 ● Grep 或 ● Read 或 ● Glob），包含工具名称和参数摘要",
          "内部工具卡片不应只是空的 Agent 外壳——Agent 卡片区域内应有具体的工具条目（每行以 ● 开头、后跟工具名）",
          // 辅助断言：位置正确
          "这些内部工具卡片应出现在 Agent 卡片区域内部，而非散落在消息区其他位置",
        ],
      });
      console.log("Judge (running internal toolcards):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 完成 —— 工具卡片痕迹保留 + 结果可见
      const r2 = await judge({
        ansiRaw: doneCapture.raw,
        criteria: [
          // 核心断言：完成后工具卡片痕迹仍存在
          "SubAgent 完成后，Agent 卡片区域内仍应保留工具调用的痕迹（如工具名称、执行计数、或 ● 标记），而非完全是空白的卡片容器",
          // 结果可见（允许统计信息形式的摘要，如匹配数/文件数）
          "Agent 工具卡片下方应有 SubAgent 完成后的结果摘要——可以是文本总结、文件列表或统计信息（如匹配数、文件数等），不应完全空白",
          // 无飘移
          "不应出现子工具调用卡片飘到 Agent 卡片上方或混入历史消息的情况",
        ],
      });
      console.log("Judge (done internal toolcards):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);
    },
  );
});
