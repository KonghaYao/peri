/**
 * 场景: 滚离底部后 streaming（§8.1，Slice 2 + Slice 5 强化）
 *
 * - 初始 FollowBottom；用户 Ctrl+Up 滚离底部 → BrowseHistory
 * - **streaming 期间**滚离底部：剩余命令 + 最终回答继续在视口下方增长，
 *   视口不得移动（§15 golden scene「滚离底部后 streaming」）
 * - 浏览态下视口未到内容底时，视口底部显示 `↓ New output` 指示器
 * - Ctrl+End 滚到底恢复跟随 → 指示器消失
 *
 * 时序设计（Slice 5 修复记录，对比旧版「turn 完成后才滚动」的空转断言）：
 * - 旧版在 turn 完成后滚动：内容已固定，「viewport 不动」是空真断言，
 *   不覆盖 §15 的「输出增长时视口不动」。新版在 streaming 中滚动。
 * - 加载信号：首个 Bash 工具卡 header（`⠇  Shell sleep 1`）出现 = agent
 *   思考结束、首批命令派发（工具卡在内容末尾，跟随态下必在视口内）。
 *   footer spinner 行无可等固定动词：loading/idle 均显示动画帧 + 随机
 *   成语占位（「思考中…」verb 无调用方渲染），elapsed 后缀 (Ns 值随
 *   时间增长无法精确匹配。
 * - 等待 2.5s（agent 思考 + 首批命令派发）后 Ctrl+Up ×3 滚离底部：此时
 *   工具卡/回答仍在下方持续增长，视口停在 prompt/早期工具卡区。
 * - 终端 40×16（transcript 视口 7 行）：滚动后视口 = [5 行 core,
 *   ↓ New output, footer 空行]——指示器可见（§8.1），动画 spinner 行在
 *   视口外；top-3 落在 prompt 末行/已完成的早期工具卡（静态）。
 * - top-3 对比前剥离每行前 3 列（outer+accent+gap）：工具卡 ◐→✓ 翻转
 *   只改符号列，不构成「viewport 移动」（内容列完全一致）。
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

/** top-3 对比归一化：剥离网格前缀列（outer+accent+gap）——运行符号
 *  ◐→✓ 翻转只影响 accent 列，不属于 viewport 移动。 */
function stripGridPrefix(lines: string[]): string[] {
  return lines.map((l) => l.slice(3));
}

describe("scenario: browse history while streaming (new output indicator)", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "streaming 期间滚离底部后 viewport 不动；指示器出现；滚底恢复跟随",
    { timeout: 300_000 },
    async () => {
      // 40×16：transcript 视口 7 行（composer 5 + status 4 = 9）。
      tester = await launchPeri({ size: { cols: 40, rows: 16 } });

      await sendPrompt(
        tester,
        "请用 Bash 依次执行 5 条命令，每条都是 sleep 1 后 echo 一个标记（用 && 连接），全部执行完不要省略：第一条 sleep 1 && echo step-1，第二条 sleep 1 && echo step-2，第三条 sleep 1 && echo step-3，第四条 sleep 1 && echo step-4，第五条 sleep 1 && echo step-5",
      );

      // 加载信号：首个 Bash 工具卡 header（`⠇  Shell sleep 1`）出现 = agent
      // 思考结束、首批命令开始派发。注意不能用「Bash」——prompt 回显即含该词
      // 会过早匹配；「Shell」是 Bash 工具卡独有文本（别名映射，§工具卡片）。
      // footer spinner 行无固定动词可等：loading 期为动画帧 + 随机成语占位
      // （idle 期同款成语，无法区分加载开始；「思考中…」verb 无调用方渲染）。
      await tester.waitForText("Shell", {
        timeout: 60_000,
        interval: 500,
      });

      // 等待 agent 思考 + 首批命令派发（sleep 1 间隔），剩余命令 + 最终
      // 回答仍会在下方持续增长（全部落在视口下方）。
      await tester.sleep(2500);

      // ── 趁 streaming 进入浏览态：Ctrl+Up ×3 滚离底部 ──
      // （步长 1 行；≥3 次脱离底部——follow 恢复判定扣除 2 行 padding）
      for (let i = 0; i < 3; i++) {
        await tester.sendKey("Up", { ctrl: true });
        await tester.sleep(400);
      }
      await tester.sleep(600);

      // 浏览态基线（顶部内容——streaming 期间 viewport 不应移动）。
      // 此刻视口 = [5 行 core（prompt 末行/早期工具卡）, ↓ New output,
      // footer 空行]：指示器可见，动画 spinner 行在视口下方。
      const baseline = await tester.getScreenText();

      // 仍在 streaming：抓拍「浏览态 + 指示器」帧（§15：滚离底部后 streaming）
      const during = await takePeriSnapshot(tester, "browse-new-output");
      expect(during.text.length).toBeGreaterThan(50);

      // 指示器可见（中英文 FTL 任一）
      const hasIndicator =
        during.text.includes("New output") || during.text.includes("新输出");
      expect(
        hasIndicator,
        `浏览态底部显示 ↓ New output：${during.text.slice(-200)}`,
      ).toBe(true);

      // 等待 turn 完成（footer 处理耗时出现 = 内容固定；期间剩余命令 +
      // 最终回答继续在视口下方增长）
      try {
        await tester.waitForText("处理耗时", {
          timeout: 150_000,
          interval: 1000,
        });
      } catch {
        await tester.waitForText("Brewed for", {
          timeout: 60_000,
          interval: 1000,
        });
      }
      await tester.sleep(800);

      // viewport 不动：streaming 期间滚离底部后，内容增长（剩余命令 +
      // 最终回答）不得移动视口——top-3 行保持（剥离网格前缀列后比对）
      const after = await tester.getScreenText();
      const topBaseline = stripGridPrefix(baseline.split("\n").slice(0, 3));
      const topAfter = stripGridPrefix(after.split("\n").slice(0, 3));
      expect(topAfter).toEqual(topBaseline);

      const r = await judge({
        ansiRaw: during.raw,
        criteria: ["视口底部显示新输出指示（'↓ New output' 或等价中文指示）"],
      });
      expect(r.pass, `指示器文本可见`).toBe(true);

      // ── Ctrl+End 滚到底恢复跟随 → 指示器消失 ──
      await tester.sendKey("End", { ctrl: true });
      await tester.sleep(1000);
      const bottom = await tester.getScreenText();
      expect(
        !bottom.includes("New output") && !bottom.includes("新输出"),
        "跟随态指示器消失（滚到底）",
      ).toBe(true);
    },
  );
});
