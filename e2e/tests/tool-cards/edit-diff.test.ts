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
import { judge } from "../../helpers/judge.js";
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
        `请用 Write 工具创建文件 ${TARGET}，内容为两行：hello 和 world`,
      );

      // 等待 turn 完成（footer 处理耗时出现；中英文环境各兜底）
      try {
        await tester.waitForText("处理耗时", {
          timeout: 180_000,
          interval: 1000,
        });
      } catch {
        await tester.waitForText("Brewed for", {
          timeout: 60_000,
          interval: 1000,
        });
      }
      await tester.sleep(1500);

      // ── 折叠态：Write 卡片 header 含路径 + 变更计数（§6.4 `+N −M`）──
      const collapsed = await takePeriSnapshot(tester, "edit-diff-collapsed");
      expect(collapsed.text.length).toBeGreaterThan(50);
      const r = await judge({
        ansiRaw: collapsed.raw,
        criteria: [
          "工具卡区域应显示 Write 头行：包含目标文件路径（e2e-edit-diff）",
          "变更摘要可见：'Wrote 2 lines' 或 '+2' 形式的行数计数",
        ],
      });
      expect(r.pass, `折叠态 diff 摘要可见`).toBe(true);

      // ── 展开 diff header：焦点移到 Write 卡（user → Write，2 次 Alt+Down）+ Enter ──
      for (let i = 0; i < 2; i++) {
        await tester.sendKey("Down", { alt: true });
        await tester.sleep(150);
      }
      await tester.sendKey("Enter");
      await tester.sleep(1000);

      const expanded = await takePeriSnapshot(tester, "edit-diff-expanded");
      expect(expanded.text.length).toBeGreaterThan(50);
      const r2 = await judge({
        ansiRaw: expanded.raw,
        criteria: [
          "展开后的 Write 工具卡显示 diff 摘要 header 行（文件路径 + '+2' 计数）",
          "文件路径（e2e-edit-diff）与 '+2' 计数可见",
        ],
      });
      expect(r2.pass, `展开态 diff header`).toBe(true);
    },
  );
});
