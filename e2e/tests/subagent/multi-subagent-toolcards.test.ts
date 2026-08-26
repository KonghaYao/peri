/**
 * 场景测试: 同 turn 多 SubAgent 时，第二个起的工具调用可见性
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
 * [Slice 3 同步] 视觉同步：SubAgentGroup 渲染为嵌套工具行（§6.7 重构后形态——
 * Agent 头行 + 缩进子工具行，无单行摘要与 `N tools` 计数）；本测试验证两个
 * SubAgent 各有一组可见工具行（非空壳），各占一行组。
 *
 * 本测试从三个维度验证：
 * A. 屏幕内容 — 两个 SubAgent 的嵌套工具行都应出现（非空壳）
 * B. 日志诊断 — 检查 agent-tui.log 中是否出现 "NOT ROUTED" 警告
 * C. 完成后状态 — 两个 SubAgent 的工具行痕迹都应保留
 */

import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
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
    "同一 turn 内主 agent 两次调用 Agent 工具，两个 SubAgent 的摘要行都应可见",
    { timeout: 600_000 },
    async () => {
      // ── Prepare: 记录测试前 NOT ROUTED 计数 ──
      const notRoutedBefore = countNotRoutedInLog();

      tester = await launchPeri();

      // 记录提交前的屏幕（用于 waitForStableScreen 基准）
      const base = await tester.getScreenText();

      // 使用 echo 让 subagent 瞬间完成任务——本测试观测的是两个 SubAgent 的
      // 单行摘要渲染（非空壳回归），不需要耗时搜索；分两步触发两个独立 SubAgent。
      await sendPrompt(
        tester,
        "请分两步完成以下任务，每一步都必须使用 Agent 工具（不可直接执行 shell）：" +
        "第一步：使用同步 general-purpose subagent，并要求它只用一次 Bash 执行 echo hello-agent-1 后立即返回。" +
        "等待第一步 Agent 工具完全返回后，第二步再使用新的同步 general-purpose subagent，" +
        "要求它只用一次 Bash 执行 echo hello-agent-2 后立即返回。" +
        "\n\n关键：每一步都必须调用 Agent 工具，不能合并。",
      );

      // ── Phase 1: 等第一个 SubAgent 的摘要行出现 ──
      await tester.waitForText("Agent", {
        timeout: 60_000,
        interval: 1000,
      });
      // 等第一个 SubAgent 的嵌套工具行出现（§6.7 重构后：`│    ✓ Shell  echo
      // hello-agent-1`；prompt 回显为小写 "shell 命令"，不会误匹配）
      await tester.waitFor(
        (screen) => /Shell\s{2}echo hello-agent-1/.test(screen),
        {
          timeout: 180_000,
          interval: 1000,
          message: "等待第一个 SubAgent 摘要行出现超时",
        },
      );

      const capture1 = await takePeriSnapshot(
        tester,
        "multi-subagent-phase1-first-running",
      );
      expect(capture1.text.length).toBeGreaterThan(50);

      // 断言 A1: 第一个 SubAgent 摘要行非空壳。
      expect(capture1.text).toMatch(/Shell\s{2}echo hello-agent-1/);

      // ── Phase 2: 等第二个 SubAgent 的真实嵌套工具行 ──
      // 不能统计裸 "Agent"：用户 prompt 自身多次包含该词，会造成立即假绿。
      try {
        await tester.waitFor(
          (screen) => /Shell\s{2}echo hello-agent-2/.test(screen),
          {
            timeout: 180_000,
            interval: 1000,
            message: "等待第二个 SubAgent 摘要行出现超时",
          },
        );
      } catch (e) {
        const diag = await tester.getScreenText();
        throw new Error(`等待第二个 SubAgent 摘要行出现超时。屏幕:\n${diag}`);
      }

      const capture2 = await takePeriSnapshot(
        tester,
        "multi-subagent-phase2-second-running",
      );
      expect(capture2.text.length).toBeGreaterThan(50);

      // 断言 A2: 两个独立嵌套工具行同时可见。
      expect(capture2.text).toMatch(/Shell\s{2}echo hello-agent-1/);
      expect(capture2.text).toMatch(/Shell\s{2}echo hello-agent-2/);

      // ── Phase 3: 等全部完成（屏幕稳定）──
      await waitForStableScreen(tester, 120_000, base);
      const capture3 = await takePeriSnapshot(
        tester,
        "multi-subagent-phase3-both-done",
      );
      expect(capture3.text.length).toBeGreaterThan(50);

      // 断言 A3: 完成后两个 SubAgent 的痕迹都保留。
      expect((capture3.text.match(/✓\s+Agent/g) || []).length).toBeGreaterThanOrEqual(2);
      expect(capture3.text).toContain("hello-agent-1");
      expect(capture3.text).toContain("hello-agent-2");

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
