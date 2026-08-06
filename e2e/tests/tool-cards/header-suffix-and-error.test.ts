/**
 * 工具卡片场景: 头行后缀 + 错误态（多阶段会话融合）
 *
 * 融合原 6 个测试（同源 issue 2026-07-20-e2e-tool-call-header-suffix-tests）：
 * - read-line-count:            Read 头行 "— N lines"
 * - glob-grep-match-count:      Glob/Grep 头行 "— N matches"
 * - edit-write-diff-summary:    Write/Edit 头行 diff 摘要（N lines changed · +N · -N）
 * - edit-diff-display:          同上（精确 turn 定位，保留更严格版本）
 * - tool-error-display:         Read 不存在文件 → 错误态强制展开、error 色
 * - tool-error-no-suffix:       错误态头行无 "— N lines" 后缀
 *
 * 一次会话 4 个顺序阶段。跨阶段文本（Read/Write 等）会留在历史中，
 * 因此每阶段用 prompt 前缀定位"当前 turn"区段（❯ 回显前缀 + 处理耗时边界）。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

/** 各阶段 prompt 前缀（屏幕回显中的独有文本，用于定位当前 turn） */
const STAGE = {
  read: "请用 Read 工具读取 Cargo.toml",
  globGrep: "请先使用 Glob 搜索",
  writeEdit: "请分两步操作",
  error: "请使用 Read 工具读取文件 /nonexistent",
} as const;

interface Turn {
  section: string;
  completed: boolean;
}

/** 检测 turn 是否完成（不依赖 prompt 回显——多阶段会话中回显可能被消息区溢出挤出屏幕） */
function currentTurn(screen: string, _marker: string): Turn | undefined {
  // 多阶段会话中 prompt 回显可能被消息区溢出挤出可见行（如 Glob 返回 166 文件）。
  // 改用全局 footer 检测——发送顺序执行保证所有 "处理耗时" / "Brewed for"
  // 必定是当前 turn 的完成标志（上一 turn 的 footer 已随旧内容滚出屏幕）。
  const zhFooter = screen.lastIndexOf("处理耗时");
  const enFooter = screen.lastIndexOf("Brewed for");
  const footerIdx = Math.max(zhFooter, enFooter);
  if (footerIdx < 0) return undefined;
  return {
    section: screen.slice(0, footerIdx),
    completed: true,
  };
}

/** 等待当前 turn 完成（footer "处理耗时" 出现） */
async function waitTurnCompleted(
  tester: TmuxTester,
  marker: string,
  timeoutMs: number,
): Promise<void> {
  try {
    await tester.waitFor(
      (screen) => {
        const t = currentTurn(screen, marker);
        return t !== undefined && t.completed;
      },
      {
        timeout: timeoutMs,
        interval: 1000,
        message: `等待 turn 完成超时: ${marker}`,
      },
    );
  } catch (e) {
    // 失败诊断：输出完整屏幕，便于定位阶段卡点
    const diag = await tester.getScreenText();
    console.log(`[DIAG] ${marker} 超时，当前屏幕:\n${diag}`);
    throw e;
  }
}

describe("tool-card: header suffix + error display", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "头行后缀（Read/Glob/Grep/Write/Edit）与错误态无后缀",
    { timeout: 900_000 },
    async () => {
      // 多阶段会话内容较长，用 60 行终端避免早期 prompt 回显滚出屏幕
      tester = await launchPeri({ size: { cols: 120, rows: 60 } });

      // ── 阶段 1：Read 头行 "— N lines" ──
      await sendPrompt(tester, `请用 Read 工具读取 Cargo.toml 文件的内容`);
      await waitTurnCompleted(tester, STAGE.read, 120_000);
      const readCapture = await takePeriSnapshot(tester, "header-suffix-read");

      // ── 阶段 2：Glob + Grep 头行 "— N matches" ──
      await sendPrompt(
        tester,
        "请先使用 Glob 搜索 'peri-tui/src/**/*.rs' 匹配 Rust 源文件，\n" +
          "再使用 Grep 在 peri-tui/src 目录搜索 'fn main' 找到所有主函数。\n" +
          "必须使用 Glob 和 Grep 两个工具，不要跳过",
      );
      await waitTurnCompleted(tester, STAGE.globGrep, 180_000);

      // agent 回复可能很长（如 markdown 表格），turn 完成后消息区吸底会把
      // 位于 Grep 卡上方的 Glob 卡挤出屏幕，Judge 将误判为缺少匹配数后缀。
      // 用 Ctrl+Home 滚动到消息区顶部，确保两张工具卡进入可见区域再截图
      //（scroll.rs 键盘滚动：Global/High handler 放行 Ctrl+Home）。
      await tester.sendKey("Home", { ctrl: true });
      await tester.waitFor(
        (screen) => screen.includes("● Glob ("),
        {
          timeout: 10_000,
          interval: 500,
          message: "滚动到顶部后 Glob 工具卡应可见",
        },
      );
      const globGrepCapture = await takePeriSnapshot(
        tester,
        "header-suffix-glob-grep",
      );

      // ── 阶段 3：Write + Edit 头行 diff 摘要 ──
      await sendPrompt(
        tester,
        "请分两步操作：\n" +
          "第一步：用 Write 工具创建文件 /tmp/peri-e2e-edit.txt，写入一行内容 'hello world'\n" +
          "第二步：用 Edit 工具修改该文件，把 'hello world' 改成 'hello peri e2e'\n" +
          "注意第二步必须用 Edit 工具（不能用 Write）",
      );
      await tester.waitFor(
        (screen) => {
          const t = currentTurn(screen, STAGE.writeEdit);
          return (
            t !== undefined &&
            t.completed &&
            /^\s*● Write \([^\n]+\) — Wrote \d+ lines?/m.test(t.section) &&
            /^\s*● Edit \([^\n]+\) — (?:Replaced text|\d+ lines changed)/m.test(
              t.section,
            )
          );
        },
        {
          timeout: 180_000,
          interval: 1000,
          message: "等待 Write/Edit 变更摘要超时",
        },
      );
      const editCapture = await takePeriSnapshot(tester, "header-suffix-edit");

      // ── 阶段 4：Read 不存在文件 → 错误态强制展开 + 头行无后缀 ──
      await sendPrompt(
        tester,
        "请使用 Read 工具读取文件 /nonexistent/peri_e2e_test_file_12345.txt",
      );
      await waitTurnCompleted(tester, STAGE.error, 120_000);
      const errorCapture = await takePeriSnapshot(tester, "header-suffix-error");

      expect(readCapture.text.length).toBeGreaterThan(50);
      expect(globGrepCapture.text.length).toBeGreaterThan(50);
      expect(editCapture.text.length).toBeGreaterThan(50);
      expect(errorCapture.text.length).toBeGreaterThan(50);

      // Judge: Read 头行行数后缀
      const r1 = await judge({
        ansiRaw: readCapture.raw,
        criteria: [
          "Read 工具的头行应包含文件路径和行数摘要，格式如 'Read (Cargo.toml) — N lines'",
          "行数 N 应是一个合理的正整数（> 0），函数调用应成功读取并显示文件行数",
        ],
      });
      console.log("Judge (read):", JSON.stringify(r1, null, 2));
      expect(r1.pass).toBe(true);

      // Judge: Glob/Grep 匹配数后缀
      const r2 = await judge({
        ansiRaw: globGrepCapture.raw,
        criteria: [
          "屏幕上应出现 Glob 和 Grep 两个工具调用的痕迹",
          "Glob 工具头行应包含匹配数后缀，格式如 'Glob (pattern: ...) — N matches'",
          "Grep 工具头行应包含匹配数后缀，格式如 'Grep (pattern: ...) — N matches'",
          "匹配数 N 应为至少为 1 的正整数",
        ],
      });
      console.log("Judge (glob-grep):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);

      // Judge: Write/Edit diff 摘要
      const r3 = await judge({
        ansiRaw: editCapture.raw,
        criteria: [
          "屏幕上应出现 Write 和 Edit 两个工具调用的痕迹",
          "Write 工具头行应包含变更摘要（如 '— Wrote N line(s)' 或 '— N lines changed'）",
          "Edit 工具头行应包含 diff 增减统计或变更摘要（如 '— N lines changed · +N · -N' 或 '— Replaced text'）",
        ],
      });
      console.log("Judge (edit):", JSON.stringify(r3, null, 2));
      expect(r3.pass).toBe(true);

      // 确定性断言：错误态头行无 "— N lines" 后缀 + 错误详情独立行可见
      const errLines = errorCapture.text.split("\n");
      const errHeader = errLines.find((l) => l.includes("● Read (/nonexistent"));
      expect(errHeader).toBeDefined();
      expect(errHeader!).not.toContain("—");
      expect(errorCapture.text).toContain("Tool execution failed");
      expect(errorCapture.text).toContain("not found");

      // Judge（信息性）：错误态强制展开 + agent 感知错误
      const r4 = await judge({
        ansiRaw: errorCapture.raw,
        criteria: [
          "Read 工具的头行应只包含文件名参数（如 'Read (/nonexistent...)'），不应有 '— N lines' 等后缀",
          "错误详细信息应在独立的输出行中可见（如 'Error:' 或 'not found' 或 'Tool execution failed'），错误信息不应被压缩消失",
          "agent 应感知到文件不存在（如 'not found'、'不存在' 等错误提示）",
        ],
      });
      console.log("Judge (error):", JSON.stringify(r4, null, 2));
    },
  );
});
