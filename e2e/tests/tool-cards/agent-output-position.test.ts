/**
 * 工具卡片场景: Agent 工具返回值显示 + 嵌套工具位置
 *
 * 对应 issue:
 * - #1 2026-07-17-agent-tool-output-not-displayed
 *   Agent 工具完成后 ToolCard 下方 output_summary 为空
 * - #6 2026-07-12-agent-nested-toolcall-misplaced-into-history
 *   子工具卡片渲染在 Agent 卡片上方，未嵌套在内部
 *
 * 验证：
 * - Agent 工具完成后的输出摘要可见（SubAgent 工具行 result + assistant 总结）
 * - 子工具不铺入主时间轴（§6.7：嵌套工具行缩进，停止递归内联）
 *
 * [Slice 3 同步] 视觉同步：subagent 从 `● Agent (…)` 卡片 + 嵌套子工具改为
 * 嵌套工具行（§6.7 重构后形态：Agent 头行 + 缩进子工具行，无单行摘要与
 * `N tools` 计数）。
 *
 * 注意：explorer subagent 需要较长时间执行（30-60s），用长等待。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

/** prompt 文本（屏幕回显中的独有文本；不依赖 ❯ 前缀——Slice 3 用户回显为 › You） */
const promptText = "请使用同步 explorer subagent（quick thoroughness）";

interface AgentTurn {
  section: string;
  completed: boolean;
}

/** footer 区域标识（加载中 spinner 动词硬编码中文；完成后为处理耗时/Brewed for）。
 *  加载期 spinner 动词行常驻 footer——`lastIndexOf` 取屏幕最后一行 footer 内容，
 *  其上方即为当前 turn 区段（发送顺序执行保证 footer 必属当前 turn）。 */
const LOADING_MARKERS = ["思考中", "执行工具", "正在生成回复"];

/** 当前 turn 区段：以全局 footer（加载 spinner / 处理耗时 / Brewed for）为边界。 */
function currentAgentTurn(screen: string): AgentTurn | undefined {
  let footerIdx = -1;
  for (const m of LOADING_MARKERS) {
    footerIdx = Math.max(footerIdx, screen.lastIndexOf(m));
  }
  footerIdx = Math.max(
    footerIdx,
    screen.lastIndexOf("处理耗时"),
    screen.lastIndexOf("Brewed for"),
  );
  if (footerIdx < 0) return undefined;
  const completed =
    screen.lastIndexOf("处理耗时") >= 0 || screen.lastIndexOf("Brewed for") >= 0;
  return {
    section: screen.slice(0, footerIdx),
    completed,
  };
}

/** SubAgent 正处于**运行中**：Agent 工具头行带 braille 动画帧符号
 *  （⠋⠙⠹…，§8.2；重构后 SubAgent 组渲染为嵌套工具行，running 符号为
 *  braille 帧而非 ◐——◐ 仅用于 reasoning `◐ Thinking…`）。
 *  [Fix race] 旧版只等「行出现」——快速 subagent 完成时抓拍的是 ✓ 完成态，
 *  judge 的 running 断言会闪红。运行态窗口内 Agent 头行符号必为 braille 帧。 */
function runningSubagentRow(section: string): boolean {
  return section
    .split("\n")
    .some((l) => /[\u2800-\u28FF]\s+Agent\s/.test(l));
}

describe("tool-card: agent output and nested position", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "Agent 工具完成后有输出且子工具不铺入主时间轴",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // 触发受限的同步 explorer subagent：保留嵌套工具卡片和结果摘要覆盖，
      // 避免全量 src 搜索导致模型长时间重复工具调用。
      await sendPrompt(
        tester,
        "请使用同步 explorer subagent（quick thoroughness），仅在 peri-tui/src/kit 中用一次 Grep 搜索 TODO 注释；完成后立即用一句中文总结，不要做额外搜索或读取。",
      );

      // 等待 SubAgent 痕迹出现且处于**运行中**（Agent 头行 braille 帧）——
      // 完成后头行符号翻转为 ✓，抓拍窗口必须落在运行态（judge 断言 running）。
      try {
        await tester.waitFor(
          (screen) => {
            const turn = currentAgentTurn(screen);
            return turn !== undefined && runningSubagentRow(turn.section);
          },
          {
            timeout: 60_000,
            interval: 500,
            message: "等待 SubAgent 运行中摘要（braille 帧 + Agent 头行）超时",
          },
        );
      } catch (e) {
        const diag = await tester.getScreenText();
        throw new Error(`等待 SubAgent 运行中摘要超时。屏幕:\n${diag}`);
      }

      const runningCapture = await takePeriSnapshot(
        tester,
        "agent-output-running",
      );

      // 等待 turn 完成（footer 出现）后再检查输出摘要。
      await tester.waitFor(
        (screen) => {
          const turn = currentAgentTurn(screen);
          return turn !== undefined && turn.completed;
        },
        {
          timeout: 180_000,
          interval: 2000,
          message: "等待同步 explorer SubAgent 和主 turn 完成超时",
        },
      );
      await tester.sleep(1000);

      const doneCapture = await takePeriSnapshot(
        tester,
        "agent-output-done",
      );

      expect(runningCapture.text.length).toBeGreaterThan(50);
      expect(doneCapture.text.length).toBeGreaterThan(50);

      // Judge: 运行中 —— SubAgent 活动摘要可见
      const r = await judge({
        ansiRaw: runningCapture.raw,
        criteria: [
          "屏幕应显示 SubAgent 正在工作的痕迹（Agent 工具行 + 嵌套子工具行，如 '⠋ Agent explorer …' 与嵌套的 Grep 行）",
          "SubAgent 内部应有嵌套子工具行（如 Grep 搜索行），表明其内部有工具调用被渲染而非空白",
          "SubAgent 相关内容应出现在用户 prompt 之后，而非上方历史消息中",
        ],
      });
      console.log("Judge (running):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 完成 —— Agent 输出 + 位置
      const r2 = await judge({
        ansiRaw: doneCapture.raw,
        criteria: [
          "SubAgent 完成后应显示完成状态（Agent 工具行成功符号 ✓）与结果摘要（如 Grep 搜索结果或 TODO 说明）",
          "如果 SubAgent 已完成，应有关于 TODO 搜索结果的文字说明——而非空白内容",
          "子工具调用不应以完整卡片形式铺入主时间轴（§6.7 停止递归内联）——它们应显示为嵌套缩进工具行，而非主时间轴平铺卡片",
        ],
      });
      console.log("Judge (done):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);
    },
  );
});
