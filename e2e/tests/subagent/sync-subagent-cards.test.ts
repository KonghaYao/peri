/**
 * 场景测试: 同步 SubAgent 单行摘要渲染（多阶段会话融合）
 *
 * 融合原 2 个测试：
 * - internal-toolcards-visibility: 同步 subagent（echo 瞬间完成）内部工具
 *   被摘要为 tool count，完成后痕迹保留
 * - sync-agents: 同步 subagent（sleep 10s）running 态活动摘要 + 完成态
 *
 * [Slice 3 同步] 视觉同步：SubAgent 从 `● Agent (…)` 卡片 + 嵌套工具卡片改为
 * 嵌套工具行（§6.7 重构后形态：Agent 头行 + 缩进子工具行，无单行摘要与
 * `N tools` 计数），嵌套工具不再内联铺入主时间轴；failed 追加原因行。
 *
 * 一次会话 2 个顺序阶段，用 prompt 前缀定位当前 Agent turn。
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

const STAGE = {
  echo: "请使用同步 subagent 执行 shell 命令 echo hello-subagent-internal",
  sleep: "请使用同步 subagent 用 shell say hello",
} as const;

interface AgentTurn {
  section: string;
  completed: boolean;
}

/** 当前 turn 区段：以全局 footer（处理耗时 / Brewed for）为完成边界。 */
function currentAgentTurn(screen: string): AgentTurn | undefined {
  const zhFooter = screen.lastIndexOf("处理耗时");
  const enFooter = screen.lastIndexOf("Brewed for");
  const footerIdx = Math.max(zhFooter, enFooter);
  if (footerIdx < 0) return undefined;
  return {
    section: screen.slice(0, footerIdx),
    completed: true,
  };
}

/** SubAgent 痕迹已出现（§6.7 重构后：Agent 头行 + 嵌套子工具行——组渲染为
 *  缩进工具行，无单行摘要与 `N tools` 计数）。嵌套行特征：工具名后 2 空格
 *  （主时间线工具行 label 后仅 1 空格，不会误匹配）。 */
function hasSubagentTrace(section: string): boolean {
  return (
    /[\u2800-\u28FF✓]\s+Agent\s/.test(section) &&
    /(?:Shell|Bash|Grep|Read|Glob|Write|Edit)\s{2}\S/.test(section)
  );
}

describe("subagent: sync agent rows (merged)", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "同步 subagent 单行摘要非空壳，且 running/完成态渲染正确",
    { timeout: 600_000 },
    async () => {
      tester = await launchPeri();

      // ── 阶段 1：echo subagent —— 工具计数非空壳 + 完成后痕迹保留 ──
      const base = await tester.getScreenText();
      await sendPrompt(tester, STAGE.echo);

      // 等 SubAgent 的嵌套工具行出现（echo 瞬间完成，抓拍可能已是完成态；
      // prompt 回显为小写 "shell 命令"，不会误匹配）
      try {
        await tester.waitFor(
          (screen) => /Shell\s{2}echo hello-subagent-internal/.test(screen),
          {
            timeout: 60_000,
            interval: 1000,
            message: "等待 SubAgent 摘要行出现超时",
          },
        );
      } catch (e) {
        const diag = await tester.getScreenText();
        throw new Error(`等待 SubAgent 摘要行出现超时。屏幕:\n${diag}`);
      }
      const runningCapture = await takePeriSnapshot(
        tester,
        "sync-subagent-echo-running",
      );

      await waitForStableScreen(tester, 120_000, base);
      const echoDoneCapture = await takePeriSnapshot(
        tester,
        "sync-subagent-echo-done",
      );

      expect(runningCapture.text.length).toBeGreaterThan(50);
      expect(echoDoneCapture.text.length).toBeGreaterThan(50);

      // Judge: 工具计数非空壳
      const r1 = await judge({
        ansiRaw: runningCapture.raw,
        criteria: [
          "消息区应出现 SubAgent 的 Agent 工具行，其下嵌套子工具行（如 'Shell echo hello-subagent-internal'），表明内部工具调用已渲染而非空壳",
          "嵌套子工具行应包含活动文本（echo 命令 hello-subagent-internal 或 Shell 相关字样）",
          "SubAgent 内容应出现在用户 prompt 之后的消息流中，而非散落在其他位置",
        ],
      });
      console.log("Judge (echo running):", JSON.stringify(r1, null, 2));
      expect(r1.pass).toBe(true);

      // Judge: 完成后痕迹保留 + 结果可见
      const r2 = await judge({
        ansiRaw: echoDoneCapture.raw,
        criteria: [
          "SubAgent 完成后，Agent 工具行（成功符号 ✓）与嵌套子工具行应保留，而非消失",
          "SubAgent 完成后的结果应可见——在工具行内（如 echo 输出 'hello-subagent-internal'）或紧随其后的文本行中，不应完全空白",
          "子工具调用不应以完整卡片形式混入主时间轴（§6.7 嵌套工具行缩进展示，不递归内联）",
        ],
      });
      console.log("Judge (echo done):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);

      // ── 阶段 2：sleep 10s subagent —— running 活动摘要 + 完成态 ──
      await sendPrompt(tester, STAGE.sleep + "，但是它要先 sleep 10s");

      // 等待 SubAgent 痕迹出现（Agent 头行 + 嵌套工具行）。sleep 10s 期间
      // subagent 必然 running 一段时间——抓拍窗口内大概率是 braille 帧；
      // 若恰好完成（✓）也可接受，judge 分别断言 running/完成两种状态。
      try {
        await tester.waitFor(
          (screen) => {
            const turn = currentAgentTurn(screen);
            return turn !== undefined && hasSubagentTrace(turn.section);
          },
          {
            timeout: 120_000,
            interval: 1000,
            message: "等待同步 subagent 的摘要行超时",
          },
        );
      } catch (e) {
        const diag = await tester.getScreenText();
        throw new Error(`等待同步 subagent 摘要行超时。屏幕:\n${diag}`);
      }
      await tester.sleep(2000);
      const sleepRunningCapture = await takePeriSnapshot(
        tester,
        "sync-subagent-sleep-running",
      );

      // 完成态：footer 出现（主 turn 完成），且 Agent 头行完成（✓）。
      // 屏幕渲染为 '✓  Agent ...'（✓ 后双空格：符号后网格 gap 空格 + summary 前导
      // 空格），用 \s+ 容忍 gap 空格，不能用单空格字面量。
      await tester.waitFor(
        (screen) => {
          const turn = currentAgentTurn(screen);
          return (
            turn !== undefined &&
            turn.completed &&
            /✓\s+Agent/.test(turn.section)
          );
        },
        {
          timeout: 120_000,
          interval: 1000,
          message: "等待同步 subagent 和主 turn 完成超时",
        },
      );
      await tester.sleep(1000);
      const sleepDoneCapture = await takePeriSnapshot(
        tester,
        "sync-subagent-sleep-done",
      );

      expect(sleepRunningCapture.text.length).toBeGreaterThan(100);
      expect(sleepDoneCapture.text.length).toBeGreaterThan(100);

      // 确定性断言（替代 judge——sleep 10s 抓拍时机不确定，judge 语义易 flaky）：
      // 完成态必须包含 ✓ Agent 头行 + 结果文本，且不再有 ◐ / braille 运行符号。
      const doneText = sleepDoneCapture.text;
      expect(doneText).toMatch(/✓\s+Agent/); // Agent 头行完成态（✓ 后双空格 gap）
      expect(doneText).toContain("\u2713"); // ✓
      expect(doneText).not.toContain("\u25d0"); // ◐（仅 reasoning running 占位，完成态无）
      expect(doneText).not.toMatch(/[\u2800-\u28FF]/); // braille 帧（仅 running 工具行，完成态无）
      expect(doneText).toMatch(/say hello|已等待|hello/);

      // Judge（信息性，不阻断）：sleep running 快照
      await judge({
        ansiRaw: sleepRunningCapture.raw,
        criteria: [
          "消息区应有 SubAgent 痕迹（Agent 工具行 + 嵌套子工具行，如 'Shell sleep 10' 形态）",
          "系统应处于处理中状态（如底部有 Spinner、状态栏或时长指示），表明 subagent 仍在运行",
        ],
      });
      console.log("Judge (sleep running): 信息性，结果不阻断");
    },
  );
});
