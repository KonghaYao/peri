/**
 * 工具卡片场景: Edit 工具 diff 显示
 *
 * 验证 Edit 完成后以头行后缀显示变更摘要：
 * - Edit 完成后头行显示 "— N lines changed · +N · -N" 后缀
 * - Write 工具创建基础文件
 * - agent 正确执行 Edit + Write 组合
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

const promptMarker = "❯ 请分两步操作：第一步：用 Write 工具创建文件";

interface EditTurn {
  section: string;
  completed: boolean;
}

function currentEditTurn(screen: string): EditTurn | undefined {
  const promptStart = screen.lastIndexOf(promptMarker);
  const writeStart =
    promptStart >= 0 ? screen.indexOf("● Write (", promptStart) : -1;
  if (writeStart < 0) {
    return undefined;
  }

  const turnEnd = screen.indexOf("处理耗时", writeStart);
  return {
    section: screen.slice(writeStart, turnEnd >= writeStart ? turnEnd : undefined),
    completed: turnEnd >= writeStart,
  };
}

function hasWriteSummary(section: string): boolean {
  return /^\s*● Write \([^\n]+\) — Wrote \d+ lines?/m.test(section);
}

function hasEditSummary(section: string): boolean {
  return /^\s*● Edit \([^\n]+\) — (?:Replaced text|\d+ lines changed)/m.test(
    section,
  );
}

describe("tool-card: edit diff display", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "Edit 成功后显示 diff 变更摘要",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      await sendPrompt(
        tester,
        "请分两步操作：\n" +
          "第一步：用 Write 工具创建文件 /tmp/peri-e2e-edit.txt，写入一行内容 'hello world'\n" +
          "第二步：用 Edit 工具修改该文件，把 'hello world' 改成 'hello peri e2e'\n" +
          "注意第二步必须用 Edit 工具（不能用 Write）",
      );

      // 等待当前 prompt 对应的 Write 卡片实际出现，避免命中 prompt / reasoning 文本。
      await tester.waitFor(
        (screen) => {
          const turn = currentEditTurn(screen);
          return turn !== undefined && hasWriteSummary(turn.section);
        },
        {
          timeout: 60_000,
          interval: 1000,
          message: "等待当前 Edit 测试 turn 的 Write 摘要超时",
        },
      );

      // 等待同一当前 turn 的 Edit 工具卡片和其变更摘要。不能把不同区域的 Edit、
      // Write 摘要或 Reasoning 文本拼接为成功条件。
      await tester.waitFor(
        (screen) => {
          const turn = currentEditTurn(screen);
          return turn !== undefined && hasWriteSummary(turn.section) && hasEditSummary(turn.section);
        },
        {
          timeout: 90_000,
          interval: 1000,
          message: "等待 Edit 工具变更摘要超时",
        },
      );
      await tester.sleep(1000);

      const editCapture = await takePeriSnapshot(tester, "edit-diff");

      // 只有当前 Edit turn 完成后才验证 agent 对编辑结果的最终确认。
      await tester.waitFor(
        (screen) => {
          const turn = currentEditTurn(screen);
          return (
            turn !== undefined &&
            turn.completed &&
            hasWriteSummary(turn.section) &&
            hasEditSummary(turn.section)
          );
        },
        {
          timeout: 90_000,
          interval: 1000,
          message: "等待 Edit 后主 turn 完成超时",
        },
      );
      await tester.sleep(1000);
      const doneCapture = await takePeriSnapshot(tester, "edit-done");

      expect(editCapture.text.length).toBeGreaterThan(50);
      expect(doneCapture.text.length).toBeGreaterThan(50);

      // Judge: Edit 阶段
      const r = await judge({
        ansiRaw: editCapture.raw,
        criteria: [
          "屏幕上应出现 Write 和 Edit 工具调用的痕迹",
          "Edit 工具的头行应显示变更摘要（如 '— N lines changed · +N · -N' 或 '— Replaced text' 格式）",
          "agent 应执行了文件编辑操作，而非跳过或用其他方式替代",
        ],
      });
      console.log("Judge (edit):", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);

      // Judge: 完成阶段
      const r2 = await judge({
        ansiRaw: doneCapture.raw,
        criteria: [
          "agent 应确认文件编辑操作已完成",
          "屏幕上应包含编辑后的内容或编辑成功的确认信息",
        ],
      });
      console.log("Judge (done):", JSON.stringify(r2, null, 2));
      expect(r2.pass).toBe(true);
    },
  );
});
