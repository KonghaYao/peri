/**
 * 场景测试: Rewind v2 回退全链路
 *
 * 双击 Esc 打开候选弹窗 → Enter 预算 → 执行回退 → 消息截断 + 输入框回填
 * → 二次双击 Esc 候选已更新。
 *
 * 覆盖的修复验证：
 * - P0: revert_files 默认 true → Write 创建的文件在执行后被删除
 * - P1 #1: history 写回 → 二次双击 Esc 候选为截断后数据（回退到第一条 → Nothing to rewind）
 * - P1 #3: 弹窗 Esc 优先级 High → Budget 视图 Esc 返回候选而非关闭弹窗
 * - P1 #9: 候选最新在前、只含 user 消息
 *
 * 文件操作在 /tmp 下进行（cwd 是项目根，避免污染仓库）。
 */
import { describe, it, expect, afterEach } from "vitest";
import fs from "node:fs";
import {
  launchPeri,
  sendPrompt,
  takePeriSnapshot,
  waitForStableScreen,
} from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

/**
 * 等待当前 turn 真正完成（全局 "处理耗时" / "Brewed for" footer 检测）。
 *
 * 不依赖 prompt 回显定位——用 lastIndexOf 可能误匹配输入框边框的会话标题，
 * 且多轮会话中早期 prompt 回显可能被后续内容推出可见行。
 * 改用全局 footer 检测：发送顺序执行保证所有 footer 属于当前 turn。
 */
async function waitTurnCompleted(
  tester: TmuxTester,
  marker: string,
  timeoutMs: number,
): Promise<void> {
  try {
    await tester.waitFor(
      (screen) => {
        return (
          screen.lastIndexOf("处理耗时") !== -1 ||
          screen.lastIndexOf("Brewed for") !== -1
        );
      },
      {
        timeout: timeoutMs,
        interval: 1000,
        message: `等待 turn 完成超时: ${marker}`,
      },
    );
  } catch (e) {
    // 诊断：超时时 dump 屏幕，便于区分"agent 仍在运行"与"turn 未渲染"
    const diag = await tester.getScreenText();
    console.error(`=== [DIAG] ${marker} turn 完成等待超时，当前屏幕（纯文本） ===`);
    console.error(diag);
    throw e;
  }
}

describe("scenarios: rewind v2 回退链路", () => {
  let tester: TmuxTester;

  // Write 工具写入、rewind 应删除的临时文件
  const targetFile = "/tmp/peri-e2e-rewind-check.txt";
  // 候选 preview 截断 200 字符，取 prompt 前缀做回填断言
  const promptPrefix = "请用 Write 工具创建文件 /tmp/peri-e2e-rewind-check.txt";

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
    try {
      fs.rmSync(targetFile, { force: true });
    } catch {}
  });

  it(
    "候选 → 预算 → 执行 → 文件删除+回填 → 二次候选更新",
    { timeout: 420_000 },
    async () => {
      // 清理上次运行遗留
      fs.rmSync(targetFile, { force: true });

      tester = await launchPeri();

      // ── 一轮对话：引导 Write 工具创建 /tmp 文件 ──
      const base = await tester.getScreenText();
      await sendPrompt(
        tester,
        `${promptPrefix}，内容写一行 hello rewind，然后用中文简短回复两个字：完成。注意必须调用 Write 工具。`,
      );
      await tester.waitForText("Write", { timeout: 90_000, interval: 1000 });
      await waitForStableScreen(tester, 180_000, base);
      // 修复竞态：agent 可能暂停超过 waitForStableScreen 的窗口（thinking /
      // 工具调用阶段之间），必须等 turn 完成（"处理耗时" footer）再双击 Esc，
      // 否则候选查询为空、弹窗显示"无可回退的消息"。
      await waitTurnCompleted(tester, promptPrefix, 180_000);
      // 等 history 写回：turn 完成事件渲染 footer 后，服务端
      // SessionState.history（prompt.rs:240）写回可能仍有延迟，
      // rewind-candidates 读到旧快照 → 候选为空。加 1s 缓冲。
      await tester.sleep(1000);

      // ── 双击 Esc 打开候选弹窗（两次间隔 <500ms 满足双击判定）──
      // 显式控制间隔：sendKeys 内部 100ms sleep + tmux exec 往返可能逼近 500ms 窗口
      await tester.sendKey("Escape");
      await tester.sleep(150);
      await tester.sendKey("Escape");
      try {
        // 语言无关：TUI 语言取决于用户机器配置（en / zh-CN），双语匹配
        await tester.waitForPattern(/回退到（\d+）|Rewind to \(\d+\)/, {
          timeout: 15_000,
          interval: 500,
          message: "双击 Esc 后应出现候选弹窗",
        });
      } catch (e) {
        // 诊断：失败时 dump 当前屏幕，区分"弹窗未开/候选为空/错误文案"
        const screen = await tester.getScreenText();
        console.error("=== 弹窗未出现，当前屏幕（纯文本） ===");
        console.error(screen);
        throw e;
      }

      const candidates = await takePeriSnapshot(tester, "rewind-candidates");
      // P1 #9: 一轮对话 → 恰好 1 个 user 候选（最新在前、不含 assistant）
      expect(candidates.text).toMatch(/回退到（1）|Rewind to \(1\)/);

      // ── Enter → 预算查询 ──
      await tester.sendKey("Enter");
      // 预算查询通过异步 ACP 请求完成；固定 sleep 可能在结果到达前截图。
      // 等待预算标题或执行态明确出现，再读取屏幕。
      await tester.waitForPattern(/回退将撤销|Rewind will revert|正在回退|Rewinding/, {
        timeout: 30_000,
        interval: 500,
        message: "等待 rewind 预算查询完成",
      });

      const screenAfterEnter = await tester.getScreenText();
      if (/回退将撤销|Rewind will revert/.test(screenAfterEnter)) {
        // 预算非空（LLM 调用了 Write）
        const budget = await takePeriSnapshot(tester, "rewind-budget");
        expect(budget.text).toContain("[write]");

        // P1 #3: Budget 视图 Esc → 返回候选视图（弹窗不关闭）
        await tester.sendKey("Escape");
        await tester.sleep(500);
        const afterEsc = await tester.getScreenText();
        expect(afterEsc).toMatch(/回退到（1）|Rewind to \(1\)/);

        // 重新进入预算并确认执行
        await tester.sendKey("Enter");
        await tester.sleep(1500);
        await tester.sendKey("Enter");
      } else {
        // 硬前置：预算为空说明 LLM 未调用 Write（或写入文件不在会话内）。
        // 空转通过会让该用例失去回归价值（file-revert 断言全部落空），
        // 这里直接 FAIL，并 dump 该轮工具调用记录（屏幕消息区）便于定位。
        console.error("=== FAIL: rewind 预算为空，未检测到 Write 工具调用 ===");
        console.error("该轮屏幕内容（含工具调用记录）:");
        console.error(screenAfterEnter);
        expect(
          screenAfterEnter,
          "预算为空：LLM 未调用 Write 工具，文件回退断言失去目标（工具调用记录见上方）",
        ).toMatch(/回退将撤销|Rewind will revert/);
      }

      // ── 等待执行完成：弹窗关闭（候选标题消失）──
      await tester.waitFor(
        (screen) => !/回退到（\d+）|Rewind to \(\d+\)/.test(screen),
        {
          timeout: 30_000,
          interval: 500,
          message: "rewind 执行完成后弹窗应关闭",
        },
      );
      // 等待 RewindCompleted 事件送达 + 输入框回填
      await tester.sleep(3000);

      // P0: Write 创建的文件应被 revert（revert_files 默认 true）
      if (fs.existsSync(targetFile)) {
        await tester.waitFor(
          async () => !fs.existsSync(targetFile),
          {
            timeout: 30_000,
            interval: 500,
            message: "rewind 后 Write 创建的文件应被删除",
          },
        );
      } else {
        // 显式标记（不静默）：走到这里预算必非空（上面硬前置已保证 Write 调用
        // 存在），目标文件缺失说明 LLM 写入了其他文件名或文件未生成——该轮
        // 删除校验被跳过，输出成因说明供人工判断，不再无声通过。
        console.error(
          `WARN: 目标文件 ${targetFile} 不存在，删除校验被跳过——` +
            "该轮 Write 未写入目标文件（实际写入路径见上方工具调用记录）",
        );
      }

      const afterExec = await takePeriSnapshot(tester, "rewind-after-exec");
      // 回退到第一条 user → 消息区截断为空；目标文本回填到输入框（preview 前 200 字符）
      expect(afterExec.text).toContain(promptPrefix);

      // Judge: 整体 UI 正常 + 回填可见
      const r = await judge({
        ansiRaw: afterExec.raw,
        criteria: [
          "屏幕底部的输入框应回填了用户刚发送的问题文本（可见其开头部分）",
          "消息区域不应再显示 agent 的完整回答（对话已被截断）",
          "不应出现渲染异常（文字覆盖、布局错位、空白闪烁残留）",
        ],
      });
      console.log("Judge (rewind-after-exec):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // ── 二次双击 Esc：候选已更新（P1 #1 history 写回）──
      // 若 SessionState.history 未写回，这里仍显示回退到（1），测试失败
      await tester.sendKey("Escape");
      await tester.sleep(150);
      await tester.sendKey("Escape");
      try {
        await tester.waitForPattern(/无可回退的消息|Nothing to rewind/, {
          timeout: 15_000,
          interval: 500,
          message: "回退到第一条后二次打开候选应为空（history 已写回）",
        });
      } catch (e) {
        // 诊断：区分"弹窗未开（双击未触发）"与"候选非空（history 未写回）"
        const screen = await tester.getScreenText();
        console.error("=== 二次双击后未出现空候选，当前屏幕（纯文本） ===");
        console.error(screen);
        throw e;
      }

      // 收尾：Esc 关闭弹窗
      await tester.sendKey("Escape");
      await tester.sleep(300);
    },
  );
});
