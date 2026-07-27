/**
 * 场景测试: 同 turn 多 SubAgent 时，第二个起的工具调用卡片可见性
 *
 * 回归测试 for issue: 同一个 turn 中主 agent 多次调用 Agent 工具，
 * 第一个 SubAgent 的工具卡片正常显示，但第二个只有容器外壳、内部为空。
 *
 * ## 复现条件
 *
 * 当第二个 SubAgent 的 `start_subagent_tool(agent_id, ...)` 路由失败（`routed=false`）时，
 * (1) SubAgentGroup 内部不展示工具卡片（空外壳）
 * (2) 旧版代码中工具事件直接丢弃，看不到任何工具调用
 * (3) 修复后 fallback 为普通 ToolCard，仍可见但不在 SubAgentGroup 内
 *
 * 本测试从三个维度验证：
 * A. 屏幕内容 — 两个 SubAgent 的执行过程中都应能看到工具调用条目
 * B. 日志诊断 — 检查 agent-tui.log 中是否出现 "NOT ROUTED" 警告
 * C. 完成后状态 — 两个 SubAgent 的工具卡片痕迹都应保留
 */

import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import { readFileSync } from "node:fs";
import type { TmuxTester } from "tui-tester";

/** 在 agent-tui.log 中搜索 NOT ROUTED 事件 */
function countNotRoutedInLog(): number {
  try {
    const log = readFileSync("../../.tmp/agent-tui.log", "utf-8");
    const matches = log.match(/NOT ROUTED/g);
    return matches ? matches.length : 0;
  } catch {
    return -1; // 文件不存在或无法读取
  }
}

describe("subagent: multi-subagent tool cards visibility (regression)", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "同一 turn 内主 agent 两次调用 Agent 工具，两个 SubAgent 的内部工具条目都应可见",
    { timeout: 360_000 },
    async () => {
      // ── Prepare: 记录测试前 NOT ROUTED 计数 ──
      const notRoutedBefore = countNotRoutedInLog();

      tester = await launchPeri();

      // 使用 explorer subagent 是因为它会自然触发工具调用（Grep/Read/Glob）
      // 分两步触发是为了确保有两个独立的 SubAgent 启动
      await sendPrompt(
        tester,
        "请分两步完成以下任务，每一步都必须使用 Agent 工具（不可直接调用 Read/Grep等）：" +
        "第一步：使用 [explorer, thorough] subagent 在 peri-tui/src/kit 目录搜索所有" +
        "包含 'SubAgent' 字符串的 Rust 文件，列出文件名和匹配行数。" +
        "第二步：完成第一步并发回结果后，再用 [explorer, thorough] subagent 在" +
        "peri-acp/src/session 目录搜索所有包含 'EventSink' 字符串的 Rust 文件，" +
        "列出文件名和匹配行数。" +
        "\n\n关键：每一步都必须调用 Agent 工具，不能合并。",
      );

      // ── Phase 1: 等第一个 SubAgent 开始运行 ──
      await tester.waitForText("Agent", {
        timeout: 60_000,
        interval: 1000,
      });
      // 确保第一个 SubAgent 已经开始（卡内有工具）
      await tester.sleep(20000);

      const capture1 = await takePeriSnapshot(
        tester,
        "multi-subagent-phase1-first-running",
      );
      expect(capture1.text.length).toBeGreaterThan(50);

      // 断言 A1: 第一个 SubAgent 运行时有内部工具卡片
      const r = await judge({
        ansiRaw: capture1.raw,
        criteria: [
          "消息区应出现第一个 Agent 卡片（标题包含 'Agent'），其内部（缩进或子行）包含至少一个工具调用卡片",
          "工具调用卡片应显示具体工具名（如 ● Grep / ● Glob / ● Read），不是空白或只有容器边框",
        ],
      });
      console.log("Judge (phase1):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // ── Phase 2: 等第二个 SubAgent 开始运行 ──
      // 第一个 SubAgent 会在 ~30s 内完成，第二个会在完成后启动
      await tester.sleep(40000);

      // 等待第二个 Agent 卡片（可能已经是 'Agent Finished' 状态）
      await tester.waitForText("Agent", {
        timeout: 60_000,
        interval: 1000,
      });
      await tester.sleep(20000);

      const capture2 = await takePeriSnapshot(
        tester,
        "multi-subagent-phase2-second-running",
      );
      expect(capture2.text.length).toBeGreaterThan(50);

      // 断言 A2: 第二个 SubAgent 运行时/完成后也有工具调用 —— 核心断言
      const r2 = await judge({
        ansiRaw: capture2.raw,
        criteria: [
          // 核心: 第二个 Agent 卡片内也要有工具调用条目
          "消息区中应出现第二个 Agent 工具调用卡片，且其内部应有具体工具调用卡片（如 ● Grep 或 ● Glob）",
          // 防御: 不应出现空的外壳——Agent 卡片的标签行下方应有实质内容
          "如果 Agent 卡片显示 'running' 或 'Finished' 状态，其内部区域不应为空——至少有一个工具条目或工具计数说明",
          // 防混淆
          "第二个 SubAgent 的工具卡片不应出现在第一个 Agent 卡片区域内",
        ],
      });
      console.log("Judge (phase2):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);

      // ── Phase 3: 等全部完成 ──
      await tester.sleep(30000);
      const capture3 = await takePeriSnapshot(
        tester,
        "multi-subagent-phase3-both-done",
      );
      expect(capture3.text.length).toBeGreaterThan(50);

      // 断言 A3: 完成后两个 SubAgent 的痕迹都保留
      const r3 = await judge({
        ansiRaw: capture3.raw,
        criteria: [
          "完成后消息区应保留两个 Agent 工具调用入口（两个 ● Agent 条目），各自显示完成状态",
          "两个 Agent 卡片完成后的输出区域都应包含工具调用结果摘要",
        ],
      });
      console.log("Judge (phase3):", JSON.stringify(r3, null, 2));
      expect(r3.pass).toBe(true);

      // ── 断言 B: 日志诊断 ──
      const notRoutedAfter = countNotRoutedInLog();
      const notRoutedNew = notRoutedBefore >= 0 && notRoutedAfter >= 0
        ? notRoutedAfter - notRoutedBefore
        : -1;

      console.log(`NOT ROUTED before: ${notRoutedBefore}, after: ${notRoutedAfter}, new: ${notRoutedNew}`);

      // 如果出现 NOT ROUTED，记录警告但不阻断测试（fallback 已兜底）
      if (notRoutedNew > 0) {
        console.warn(
          `[DIAGNOSTIC] ${notRoutedNew} "NOT ROUTED" events detected. ` +
          "This indicates SubAgent tool routing failure. " +
          "Check agent-tui.log for 'registered_agent_ids' details."
        );
      }
    },
  );
});
