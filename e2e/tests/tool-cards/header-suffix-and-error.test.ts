/**
 * 工具卡片场景: 头行后缀 + 错误态（多阶段会话融合）
 *
 * 融合原 6 个测试（同源 issue 2026-07-20-e2e-tool-call-header-suffix-tests）：
 * - read-line-count:            Read 头行 "— N lines"
 * - glob-grep-match-count:      Glob/Grep 头行 "— N matches"
 * - edit-write-diff-summary:    Write/Edit 头行 diff 计数（· +N · -N）
 * - edit-diff-display:          同上（精确 turn 定位，保留更严格版本）
 * - tool-error-display:         Read 不存在文件 → 错误态默认折叠、显式展开详情
 * - tool-error-no-suffix:       错误态头行无 "— N lines" 后缀
 *
 * 一次会话 4 个顺序阶段。跨阶段文本（Read/Write 等）会留在历史中，
 * 因此每阶段用 prompt 前缀定位"当前 turn"区段（› 回显前缀 + 处理耗时边界）。
 *
 * [Slice 3] 视觉同步：tool 行从 `● Read (path)` 卡片头改为统一网格单行
 * `✓ Read  {path} — N lines`（§6.4：符号 + label + 摘要 + 后缀）。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
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
      await sendPrompt(tester, `请用 Read 工具读取 Cargo.toml 文件的内容，完成后只回复已读取，不要总结文件`);
      await waitTurnCompleted(tester, STAGE.read, 120_000);
      const readCapture = await takePeriSnapshot(tester, "header-suffix-read");

      // ── 阶段 2：Glob + Grep 头行 "— N matches" ──
      await sendPrompt(
        tester,
        "请先使用 Glob 搜索 'peri-tui/src/**/*.rs' 匹配 Rust 源文件，\n" +
          "再使用 Grep 在 peri-tui/src 目录搜索 'fn main' 找到所有主函数。\n" +
          "必须使用 Glob 和 Grep 两个工具，不要跳过。完成后只回复已搜索，不要罗列结果",
      );
      await waitTurnCompleted(tester, STAGE.globGrep, 180_000);

      // 回复长度与工具聚合都会改变卡片位置；从顶部扫描真实工具行，
      // 不把包含 Glob 的 prompt 回显当作卡片已经进入视口。
      await tester.sendKey("Home", { ctrl: true });
      let foundMatchCounts = false;
      for (let scan = 0; scan < 40; scan++) {
        const screen = await tester.getScreenText();
        const groupRow = screen.split("\n")
          .findIndex((line) => /▸.*Glob \d+.*Grep \d+/.test(line));
        if (groupRow >= 0) {
          await tester.click(4, groupRow);
          await tester.sendText(`\u001b[<0;5;${groupRow + 1}m`);
          continue;
        }
        if (/✓[ ]+Glob[^\n]*— [1-9]\d* matches/.test(screen)
          && /✓[ ]+Grep[^\n]*— [1-9]\d* matches/.test(screen)) {
          foundMatchCounts = true;
          break;
        }
        for (let line = 0; line < 5; line++) {
          await tester.sendKey("Down", { ctrl: true });
        }
      }
      expect(foundMatchCounts, "扫描消息区后应看见 Glob/Grep 各自的匹配数").toBe(true);
      const globGrepCapture = await takePeriSnapshot(
        tester,
        "header-suffix-glob-grep",
      );
      // 阶段 2 为查看 Glob 卡片滚到了顶部；恢复到底部后再提交下一阶段，
      // 让后续 turn 的完成 footer 和工具卡保持在当前消息视口。
      await tester.sendKey("End", { ctrl: true });
      await tester.sleep(200);

      // ── 阶段 3：Write + Edit 头行 diff 摘要 ──
      await sendPrompt(
        tester,
        "请分两步操作：\n" +
          "第一步：用 Write 工具创建文件 /tmp/peri-e2e-edit.txt，写入一行内容 'hello world'\n" +
          "第二步：用 Edit 工具修改该文件，把 'hello world' 改成 'hello peri e2e'\n" +
          "注意第二步必须用 Edit 工具（不能用 Write）",
      );
      // 等待 Write/Edit 变更摘要：独立行（`✓ Write path · +N` 计数后缀——
      // §6.4 摘要文本含路径不重复拼接；符号后为网格 gap + 前导空格，用 `[ ]+`
      // 容忍）或 §7 分组聚合行（`Write 1 · Edit 1`，含 diff 工具不合并、不分组）
      try {
        await tester.waitFor(
          (screen) => {
            const t = currentTurn(screen, STAGE.writeEdit);
            return (
              t !== undefined &&
              t.completed &&
              /(?:✓[ ]+Write\b[^\n]*|Write \d+)/m.test(t.section) &&
              /(?:✓[ ]+Edit\b[^\n]*|Edit \d+)/m.test(t.section)
            );
          },
          {
            timeout: 180_000,
            interval: 1000,
            message: "等待 Write/Edit 变更摘要超时",
          },
        );
      } catch (error) {
        // 失败时也保存当前终端，区分输入未送达、工具未执行和摘要渲染问题。
        await takePeriSnapshot(tester, "header-suffix-edit-failure");
        await tester.sendKey("End", { ctrl: true });
        await tester.sleep(200);
        await takePeriSnapshot(tester, "header-suffix-edit-failure-bottom");
        throw error;
      }
      const editCapture = await takePeriSnapshot(tester, "header-suffix-edit");

      // ── 阶段 4：Read 不存在文件 → 错误态默认折叠 + 头行无后缀 ──
      await sendPrompt(
        tester,
        "请使用 Read 工具读取文件 /nonexistent/peri_e2e_test_file_12345.txt",
      );
      await waitTurnCompleted(tester, STAGE.error, 120_000);
      const errorCollapsedCapture = await takePeriSnapshot(
        tester,
        "header-suffix-error-collapsed",
      );

      // 点击工具卡首行切换折叠。
      // 错误态 header 始终保持 `×`，通过详细错误输出判断展开状态。
      const collapsedScreen = await tester.getScreenText();
      const errorRow = collapsedScreen
        .split("\n")
        .findIndex((line) => line.includes("×") && line.includes("/nonexistent"));
      expect(errorRow, "错误工具卡应位于当前视口").toBeGreaterThanOrEqual(0);
      await tester.click(4, errorRow);
      // tui-tester.click 当前只发 SGR Down；Peri 在 Up 时提交单击动作。
      await tester.sendText(`\u001b[<0;5;${errorRow + 1}m`);
      try {
        await tester.waitFor(
          (screen) => /×[^\n]*Read[^\n]*\/nonexistent[^\n]*\n[^\n]*Error: File not found at \/nonexistent/.test(screen),
          { timeout: 5_000, interval: 200, message: "点击错误工具卡后应显示详细错误" },
        );
      } catch (error) {
        await takePeriSnapshot(tester, "header-suffix-error-expand-failure").catch(() => {});
        throw error;
      }
      const errorExpandedCapture = await takePeriSnapshot(
        tester,
        "header-suffix-error-expanded",
      );

      expect(readCapture.text.length).toBeGreaterThan(50);
      expect(globGrepCapture.text.length).toBeGreaterThan(50);
      expect(editCapture.text.length).toBeGreaterThan(50);
      expect(errorCollapsedCapture.text.length).toBeGreaterThan(50);
      expect(errorExpandedCapture.text.length).toBeGreaterThan(50);

      // 这些后缀是确定的终端文本，直接核验工具卡，不依赖 Judge 生成 JSON。
      expect(readCapture.text).toMatch(/✓[ ]+Read[^\n]*Cargo\.toml — [1-9]\d* lines/);
      expect(globGrepCapture.text).toMatch(/✓[ ]+Glob[^\n]*— [1-9]\d* matches/);
      expect(globGrepCapture.text).toMatch(/✓[ ]+Grep[^\n]*— [1-9]\d* matches/);
      expect(editCapture.text).toMatch(/✓[ ]+Write[^\n]*· \+[1-9]\d*/);
      expect(editCapture.text).toMatch(/✓[ ]+Edit[^\n]*· \+[1-9]\d* · -[1-9]\d*/);

      // 确定性断言：错误态终态默认折叠，头行无 "— N lines" 后缀且明确错误词。
      // 选择器必须命中**工具卡错误行**而非 prompt 回显——回显行
      // （`请使用 Read 工具读取文件 /nonexistent/...`）同样含 "Read" 与路径，
      // 用错误符号 ×（§8.2 错误态符号）限定。
      const errLines = errorCollapsedCapture.text.split("\n");
      const errHeader = errLines.find(
        (l) => l.includes("×") && l.includes("/nonexistent"),
      );
      expect(errHeader).toBeDefined();
      expect(errHeader!).not.toMatch(/—\s*\d+\s*lines/);
      // 错误词为 i18n 本地化（msg-status-failed：zh-CN「— 失败」/ en「— Failed」）
      expect(
        errHeader!.includes("失败") || errHeader!.includes("Failed"),
        `错误态头行含错误词：${errHeader}`,
      ).toBe(true);
      const expandedError = /×[^\n]*Read[^\n]*\/nonexistent[^\n]*\n[^\n]*Error: File not found at \/nonexistent/;
      expect(errorCollapsedCapture.text).not.toMatch(expandedError);
      expect(errorExpandedCapture.text).toMatch(expandedError);

    },
  );
});
