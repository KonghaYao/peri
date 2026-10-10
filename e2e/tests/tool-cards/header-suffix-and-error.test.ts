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
 * 因此每阶段使用工具结果实际回到模型后的唯一回复与处理耗时定位完成边界。
 *
 * [Slice 3] 视觉同步：tool 行从 `● Read (path)` 卡片头改为统一网格单行
 * `✓ Read  {path} — N lines`（§6.4：符号 + label + 摘要 + 后缀）。
 */
import { describe, it, expect, afterEach } from "vitest";
import { sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import {
  launchReplayTui, makeReplayHome, replayDebug, startReplayModelServer, type ReplayServer,
} from "../../helpers/replay-model.js";
import type { TmuxTester } from "tui-tester";

/** 各阶段 prompt 前缀，用于匹配真实模型请求的重放步骤。 */
const STAGE = {
  read: "请用 Read 工具读取 Cargo.toml",
  globGrep: "请先使用 Glob 搜索",
  writeEdit: "请分两步操作",
  error: "请使用 Read 工具读取文件 /nonexistent",
} as const;

const DONE = {
  read: "HEADER_READ_COMPLETE",
  globGrep: "HEADER_SEARCH_COMPLETE",
  writeEdit: "HEADER_DIFF_COMPLETE",
  error: "HEADER_ERROR_COMPLETE",
} as const;

/**
 * 错误卡展开后可见的详情文本（§8.2 错误态展开）。
 *
 * 展开后上屏的是 MCP 桥接层的脱敏错误文本——`/nonexistent/...` 路径只出现在
 * 卡片 header（input_summary），详情行只含桥接消息且被硬截断为单行（尾部 `…`）：
 *   ×  Read /nonexistent/peri_e2e_test_file_12345.txt — Failed
 *   │  MCP 服务器 "workspace" 工具 "Read" 调用失败: tool `Read` failed to execute; File not found. Verify f…
 *
 * 断言文本来源：`failed to execute`（mcp-packages/common/src/result_mapping.rs:25）
 * 与 `File not found`（mcp-packages/workspace/src/filesystem/read.rs:218）。
 * 原始 `Error: File not found at /nonexistent/...`（ToolFailure.detail，私有字段）
 * 按脱敏策略不上屏，仅保留为旧形态备选分支。
 *
 * waitFor 谓词与最终断言必须共用此常量，否则展开/折叠判定会不同源而互相矛盾。
 */
const EXPANDED_ERROR_RE =
  /×[^\n]*Read[^\n]*\/nonexistent[^\n]*\n[^\n]*(?:failed to execute[^\n]*File not found|Error: File not found at)/;

interface Turn {
  section: string;
  completed: boolean;
}

/** 检测 turn 是否完成（不依赖 prompt 回显——多阶段会话中回显可能被消息区溢出挤出屏幕） */
function currentTurn(screen: string, marker: string): Turn | undefined {
  const zhFooter = screen.lastIndexOf("处理耗时");
  const enFooter = screen.lastIndexOf("Brewed for");
  const footerIdx = Math.max(zhFooter, enFooter);
  if (footerIdx < 0 || screen.lastIndexOf(marker) < 0
    || screen.lastIndexOf(marker) > footerIdx) return undefined;
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
  let replay: ReplayServer | undefined;
  let home: string | undefined;
  let fixture: string | undefined;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
    await replay?.close();
    if (home) await rm(home, { recursive: true, force: true });
    if (fixture) await rm(fixture, { recursive: true, force: true });
  });

  it(
    "头行后缀（Read/Glob/Grep/Write/Edit）与错误态无后缀",
    { timeout: 900_000 },
    async () => {
      // 多阶段会话内容较长，用 60 行终端避免早期 prompt 回显滚出屏幕
      fixture = await mkdtemp(path.join(os.tmpdir(), "peri-header-"));
      const cargo = path.join(fixture, "Cargo.toml");
      const edited = path.join(fixture, "edit.txt");
      await writeFile(cargo, '[package]\nname = "header-fixture"\nversion = "0.1.0"\nedition = "2021"\n');
      await writeFile(path.join(fixture, "first.rs"), "fn main() {}\n");
      await writeFile(path.join(fixture, "second.rs"), "fn main() {}\n");
      replay = await startReplayModelServer([
        { prompt: STAGE.read, toolUses: [{ name: "Read", input: { file_path: cargo } }] },
        { text: DONE.read },
        { prompt: STAGE.globGrep, toolUses: [
          { name: "Glob", input: { pattern: "*.rs", path: fixture } },
          { name: "Grep", input: { pattern: "fn main", path: fixture, output_mode: "content" } },
        ] },
        { text: DONE.globGrep },
        { prompt: STAGE.writeEdit, toolUses: [
          { name: "Write", input: { file_path: edited, content: "hello world\n" } },
        ] },
        { toolUses: [
          { name: "Edit", input: { file_path: edited, old_string: "hello world", new_string: "hello peri e2e" } },
        ] },
        { text: DONE.writeEdit },
        { prompt: STAGE.error, toolUses: [
          { name: "Read", input: { file_path: "/nonexistent/peri_e2e_test_file_12345.txt" } },
        ] },
        { text: DONE.error },
      ]);
      home = await makeReplayHome(replay.port);
      tester = await launchReplayTui({ home, cwd: fixture, size: { cols: 120, rows: 60 } });

      // ── 阶段 1：Read 头行 "— N lines" ──
      await sendPrompt(tester, `请用 Read 工具读取 Cargo.toml 文件的内容，完成后只回复已读取，不要总结文件`);
      await waitTurnCompleted(tester, DONE.read, 120_000);
      const readCapture = await takePeriSnapshot(tester, "header-suffix-read");

      // ── 阶段 2：Glob + Grep 头行 "— N matches" ──
      await sendPrompt(
        tester,
        `请先使用 Glob 搜索 '${fixture}/*.rs' 匹配 Rust 源文件，\n` +
          `再使用 Grep 在 ${fixture} 目录搜索 'fn main' 找到所有主函数。\n` +
          "必须使用 Glob 和 Grep 两个工具，不要跳过。完成后只回复已搜索，不要罗列结果",
      );
      await waitTurnCompleted(tester, DONE.globGrep, 180_000);

      // 回复长度与工具聚合都会改变卡片位置；从顶部扫描真实工具行，
      // 不把包含 Glob 的 prompt 回显当作卡片已经进入视口。
      await tester.sendKey("Home", { ctrl: true });
      let foundMatchCounts = false;
      for (let scan = 0; scan < 40; scan++) {
        const screen = await tester.getScreenText();
        if (/✓[ ]+Glob[^\n]*— [1-9]\d* matches/.test(screen)
          && /✓[ ]+Grep[^\n]*— [1-9]\d* matches/.test(screen)) {
          foundMatchCounts = true;
          break;
        }
        const groupRow = screen.split("\n")
          .findIndex((line) => /▸[^\n]*(?:Glob|Grep) \d+/.test(line));
        if (groupRow >= 0) {
          const column = screen.split("\n")[groupRow].indexOf("▸");
          await tester.click(column, groupRow);
          await tester.sendText(`\u001b[<0;${column + 1};${groupRow + 1}m`);
          continue;
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
          `第一步：用 Write 工具创建文件 ${edited}，写入一行内容 'hello world'\n` +
          "第二步：用 Edit 工具修改该文件，把 'hello world' 改成 'hello peri e2e'\n" +
          "注意第二步必须用 Edit 工具（不能用 Write）",
      );
      // 等待 Write/Edit 变更摘要：独立行（`✓ Write path · +N` 计数后缀——
      // §6.4 摘要文本含路径不重复拼接；符号后为网格 gap + 前导空格，用 `[ ]+`
      // 容忍）或 §7 分组聚合行（`Write 1 · Edit 1`，含 diff 工具不合并、不分组）
      try {
        await tester.waitFor(
          (screen) => {
            const t = currentTurn(screen, DONE.writeEdit);
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
      await waitTurnCompleted(tester, DONE.error, 120_000);
      const errorCollapsedCapture = await takePeriSnapshot(
        tester,
        "header-suffix-error-collapsed",
      );

      // 点击工具卡首行切换折叠。
      // 错误态 header 始终保持 `×`（`× Read {path} — Failed`），无法靠符号判断展开状态，
      // 只能通过详情行判断：展开后新增的是 MCP 桥接层的单行脱敏错误文本
      // （`MCP 服务器 "workspace" 工具 "Read" 调用失败: tool \`Read\` failed to execute; File not found. Verify f…`）。
      // 注意 `/nonexistent` 路径只出现在卡片 header（input_summary），详情行尾部被硬截断（`…`）。
      const collapsedScreen = await tester.getScreenText();
      const errorRow = collapsedScreen
        .split("\n")
        .findIndex((line) => line.includes("×") && line.includes("/nonexistent"));
      expect(errorRow, "错误工具卡应位于当前视口").toBeGreaterThanOrEqual(0);
      await tester.click(4, errorRow);
      // tui-tester.click 当前只发 SGR Down；Peri 在 Up 时提交单击动作。
      // 展开判定谓词与下方终态断言共用 EXPANDED_ERROR_RE，保证两者同源不漂移。
      await tester.sendText(`\u001b[<0;5;${errorRow + 1}m`);
      try {
        await tester.waitFor(
          (screen) => EXPANDED_ERROR_RE.test(screen),
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
      // 折叠态必须不含详情行、展开态必须命中（与 waitFor 谓词同一正则）。
      expect(errorCollapsedCapture.text).not.toMatch(EXPANDED_ERROR_RE);
      expect(errorExpandedCapture.text).toMatch(EXPANDED_ERROR_RE);
      expect(await readFile(edited, "utf8")).toBe("hello peri e2e\n");
      expect(replay.misses(), replayDebug(replay)).toBe(0);
      const results = new Map<string, { content: unknown; is_error?: boolean }>();
      for (const request of replay.requests) {
        for (const message of request.body?.messages ?? []) {
          if (!Array.isArray(message.content)) continue;
          for (const block of message.content) {
            if (block.type === "tool_result") results.set(block.tool_use_id, block);
          }
        }
      }
      for (const id of ["replay-1-0", "replay-3-0", "replay-3-1", "replay-5-0", "replay-6-0"]) {
        expect(results.get(id), `真实工具结果应回到模型请求：${id}`).toBeDefined();
        expect(results.get(id)?.is_error, `成功工具不得返回错误：${id}`).not.toBe(true);
        expect((JSON.stringify(results.get(id)?.content) ?? "").length).toBeGreaterThan(2);
      }
      expect(JSON.stringify(results.get("replay-1-0")?.content)).toContain("header-fixture");
      expect(JSON.stringify(results.get("replay-3-0")?.content)).toContain("first.rs");
      expect(JSON.stringify(results.get("replay-3-1")?.content)).toContain("fn main");
      expect(results.get("replay-8-0")?.is_error).toBe(true);
      expect(JSON.stringify(results.get("replay-8-0")?.content)).toContain("File not found");

    },
  );
});
