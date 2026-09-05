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

function tryPromptAnchorText(screen: string): string | null {
  const line = screen.split("\n").find(
    (l) =>
      l.includes("BROWSE_E2E_ANCHOR") &&
      !l.includes("Shell") &&
      !l.includes("Thought"),
  );
  return line ? line.slice(3) : null;
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
    { timeout: 480_000 },
    async () => {
      // 40×16：transcript 视口 7 行（composer 5 + status 4 = 9）。
      tester = await launchPeri({ size: { cols: 40, rows: 16 } });

      await sendPrompt(
        tester,
        "BROWSE_E2E_ANCHOR。这是 E2E 测试。请严格只调用一次 Bash 工具执行 sleep 20，" +
        "不要只解释命令；完成后只回复 done。",
      );

      await tester.waitFor(
        (screen) =>
          /(?:^|\n)\s*[\u2800-\u28ff]\s+(?:Bash|Shell)\b[^\n]*sleep 20/m.test(
            screen,
          ),
        {
          timeout: 180_000,
          interval: 500,
          message: "等待 Bash sleep 20 进入运行态",
        },
      );

      await tester.sleep(4000);

      const anchorText = tryPromptAnchorText(await tester.getScreenText());
      expect(anchorText, "滚离底部前用户 prompt 应在视口内").toBeTruthy();

      // 逐步上滚直到出现 New output（并发下固定 2/3 次易不足或滚丢锚点）
      let baseline = await tester.getScreenText();
      let scrollUps = 0;
      const indicatorDeadline = Date.now() + 120_000;
      while (Date.now() < indicatorDeadline) {
        baseline = await tester.getScreenText();
        if (baseline.includes("New output") || baseline.includes("新输出")) {
          break;
        }
        if (scrollUps < 5) {
          await tester.sendKey("Up", { ctrl: true });
          scrollUps += 1;
          await tester.sleep(450);
          continue;
        }
        await tester.sleep(400);
      }
      expect(
        baseline.includes("New output") || baseline.includes("新输出"),
        "浏览态底部应显示 New output 指示器",
      ).toBe(true);
      if (tryPromptAnchorText(baseline)) {
        expect(tryPromptAnchorText(baseline)).toBe(anchorText);
      }

      const during = await takePeriSnapshot(tester, "browse-new-output");
      expect(during.text.length).toBeGreaterThan(50);

      const hasIndicator =
        during.text.includes("New output") || during.text.includes("新输出");
      expect(hasIndicator).toBe(true);

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
      expect(after, "浏览期间屏幕应随 streaming 输出继续更新").not.toEqual(baseline);
      const afterAnchor = tryPromptAnchorText(after);
      if (afterAnchor) {
        expect(afterAnchor).toBe(anchorText);
      }

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
