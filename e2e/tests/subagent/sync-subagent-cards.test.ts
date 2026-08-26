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
import type { TmuxTester } from "tui-tester";

const STAGE = {
  echo:
    "请使用同步 general-purpose subagent；它必须且只用一次 Bash 执行 echo hello-subagent-internal，然后立即返回结果。不要由主 agent 执行 shell。",
  sleep:
    "请使用新的同步 general-purpose subagent；它必须且只用一次 Bash 执行 sleep 10 && echo hello-sync-sleep，然后立即返回结果。不要由主 agent 执行 shell。",
} as const;

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
            timeout: 180_000,
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

      expect(runningCapture.text).toMatch(/Shell\s{2}echo hello-subagent-internal/);
      expect(echoDoneCapture.text).toMatch(/✓\s+Agent/);
      expect(echoDoneCapture.text).toContain("hello-subagent-internal");

      // ── 阶段 2：sleep 10s subagent —— running 活动摘要 + 完成态 ──
      await sendPrompt(tester, STAGE.sleep);

      // 等待 SubAgent 痕迹出现（Agent 头行 + 嵌套工具行）。sleep 10s 期间
      // subagent 必然 running 一段时间——抓拍窗口内大概率是 braille 帧；
      // 若恰好完成（✓）也可接受，judge 分别断言 running/完成两种状态。
      try {
        await tester.waitFor(
          (screen) => /Shell\s{2}sleep 10 && echo hello-sync-sleep/.test(screen),
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
            return (
              /✓\s+Agent/.test(screen) &&
              /✓\s+Shell\s{2}sleep 10 && echo hello-sync-sleep/.test(screen) &&
              /(?:Brewed for|处理耗时)/.test(screen)
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
      expect(doneText).toContain("hello-sync-sleep");
    },
  );
});
