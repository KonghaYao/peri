/**
 * 工具卡片场景: Edit/Write diff 摘要渲染（§6.5，G-Diff，Slice 5）
 *
 * 数据事实（Slice 5 适配记录）：事件流中 Edit/Write 工具的 `output_summary`
 * 是摘要文本（`Wrote N lines to P` / `Added N lines to P`），**不含 unified
 * diff**——`parse_unified_diff` 对真实摘要恒返回 None。TUI 侧 `parse_tool_diff`
 * 在 unified 解析失败后回退摘要解析（`parse_edit_write_summary`）：提取
 * `+N`/`−M` 计数构造无 hunk 的 diff 块。本场景用 Write 创建新文件（输出恒为
 * `Wrote N lines`，计数稳定可达）验证：
 *
 * - 折叠态 header：`· +2` 计数（§6.4 `+N −M` 口径；摘要文本含路径不重复拼接）
 * - Alt+Down 焦点到 Write 卡 + Enter 展开 → diff header 行 `path +N`
 *   （hunk 渲染仅单元测试覆盖——真实协议无 hunk 数据，§6.5 golden 的
 *   120/80/48 列由 render_test 锁定）
 *
 * [Slice 5] 新增场景（§15 golden scene：diff 的 120 列）。
 */
import { describe, it, expect, afterEach } from "vitest";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import type { TmuxTester } from "tui-tester";

/** 每次运行独立目标文件（避免并行/重跑冲突）。 */
const TARGET = path.join(os.tmpdir(), `peri-e2e-edit-diff-${process.pid}.txt`);

describe("tool-card: edit/write diff summary rendering (G-Diff)", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
    try {
      fs.unlinkSync(TARGET);
    } catch {
      /* 文件可能已被 agent 删除 */
    }
  });

  it(
    "Write 新文件 diff 摘要：折叠 +N 计数 + 展开 diff header",
    { timeout: 300_000 },
    async () => {
      // 目标文件必须不存在（Write 创建新文件 → `Wrote N lines` 输出）
      try {
        fs.unlinkSync(TARGET);
      } catch {
        /* 不存在即预期 */
      }

      tester = await launchPeri({ size: { cols: 120, rows: 60 } });

      await sendPrompt(
        tester,
        `这是 E2E 测试。请立即且只调用一次 Write 工具，file_path 必须是 ${TARGET}，content 必须严格为两行 hello 和 world；成功后只回复“完成”，不得再次调用 Write。`,
      );

      await tester.waitForPattern(/✓\s+Write .*e2e-edit-diff.*· \+2/, {
        timeout: 120_000,
        interval: 500,
        message: "等待单次 Write +2 完成态",
      });
      await tester.waitFor(
        (screen) => /(?:Brewed for|处理耗时)/.test(screen),
        { timeout: 120_000, interval: 500, message: "等待 Write 主 turn 完成" },
      );

      // ── 折叠态：Write 卡片 header 含路径 + 变更计数（§6.4 `+N −M`）──
      const collapsed = await takePeriSnapshot(tester, "edit-diff-collapsed");
      expect(collapsed.text.length).toBeGreaterThan(50);
      expect(collapsed.text).toMatch(/✓\s+Write .*e2e-edit-diff.*· \+2/);

      // ── 展开 diff header：从末尾 entry 向上遍历，直到目标 Write 卡首行
      // 变为展开符号 ▾。固定次数导航会因 reasoning/tool 交错数量变化而命中
      // 错误 entry；公开符号是更可靠的因果确认。
      let expandedWrite = false;
      for (let i = 0; i < 12; i++) {
        await tester.sendKey("Up", { alt: true });
        await tester.sleep(100);
        await tester.sendKey("Enter");
        await tester.sleep(250);
        if (/▾\s+Write .*e2e-edit-diff.*· \+2/.test(await tester.getScreenText())) {
          expandedWrite = true;
          break;
        }
      }
      expect(expandedWrite, "应能聚焦并展开目标 Write 卡").toBe(true);

      const expanded = await takePeriSnapshot(tester, "edit-diff-expanded");
      expect(expanded.text.length).toBeGreaterThan(50);
      expect(expanded.text).toMatch(/▾\s+Write .*e2e-edit-diff.*· \+2/);
    },
  );
});
