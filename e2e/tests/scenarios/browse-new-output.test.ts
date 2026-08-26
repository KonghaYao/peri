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
 * - 加载信号：唯一 Bash 工具卡 header（`⠇  Shell for i ...`）出现 = agent；
 *   12 秒循环为滚离底部并建立基线后保留充足的真实输出增长窗口。
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
import { launchPeri, sendPrompt, takePeriSnapshot, waitForStableScreen } from "../../helpers/peri.js";
import type { TmuxTester } from "tui-tester";

/** 从真实消息区选择不可变的用户 prompt 行作为 viewport 锚点。
 * 排除工具卡和动态状态行，避免把 spinner、elapsed 或折叠状态变化误判为滚动。 */
function findPromptAnchor(screen: string): { text: string; row: number } {
  const lines = screen.split("\n");
  const row = lines.findIndex(
    (line) =>
      line.includes("BROWSE_E2E_ANCHOR") &&
      !line.includes("Shell") &&
      !line.includes("Thought"),
  );
  expect(row, "浏览态应显示不可变的用户 prompt 锚点").toBeGreaterThanOrEqual(0);
  return { text: lines[row].slice(3), row };
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
        "BROWSE_E2E_ANCHOR。这是 E2E 测试。请立即且只调用一次 Bash 工具，" +
        "command 参数必须严格为 for i in 1 2 3 4 5 6 7 8 9 10 11 12; do sleep 1; echo step-$i; done，" +
        "run_in_background=false；不得使用 Agent、不得拆分命令或只解释。工具完成后只回复 done。",
      );

      // 加载信号：首个 Bash 工具卡 header（`⠇  Shell sleep 1`）出现 = agent
      // 思考结束、首批命令开始派发。注意不能用「Bash」——prompt 回显即含该词
      // 会过早匹配；「Shell」是 Bash 工具卡独有文本（别名映射，§工具卡片）。
      // footer spinner 行无固定动词可等：loading 期为动画帧 + 随机成语占位
      // （idle 期同款成语，无法区分加载开始；「思考中…」verb 无调用方渲染）。
      await tester.waitForText("Shell", {
        // provider 首次 reasoning 在真实串行套件中可超过 60s；Shell 出现仍是
        // 唯一因果入口，放宽等待不改变 streaming 断言本身。
        timeout: 180_000,
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

      // 浏览态基线：选择真实、不可变的用户消息行作为屏幕位置锚点。
      const baseline = await tester.getScreenText();
      const anchor = findPromptAnchor(baseline);

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

      // 等待视口稳定：浏览态（滚离底部）下 footer 不渲染——视口裁剪只在
      // 贴底时附加 footer 行（mod.rs viewport_has_footer：scroll_y + vp_height
      // > core_total_visual_rows），「处理耗时/Brewed for」文本在浏览态不可见，
      // 不能作 turn 完成信号。§15 断言也不依赖 turn 完成：
      // - after 对比（视口不动）发生在 streaming 中抓取，验证的正是「输出
      //   增长时视口不动」（非空真断言）；
      // - 「↓ New output」指示器消失取决于贴底跟随（follow_bottom），与
      //   turn 是否完成无关。
      await waitForStableScreen(tester, 120_000);

      // viewport 不动：不可变的用户 prompt 行在输出继续增长并最终稳定后，
      // 仍位于同一屏幕行。动态工具卡、spinner、elapsed 和 footer 不参与比较。
      const after = await tester.getScreenText();
      const afterLines = after.split("\n");
      expect(after, "浏览期间屏幕应随 streaming 输出继续更新").not.toEqual(baseline);
      expect(afterLines[anchor.row]?.slice(3)).toBe(anchor.text);

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
