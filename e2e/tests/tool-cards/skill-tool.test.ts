/**
 * 测试 Skill 工具：Agent 调用 Skill Tool 加载 skill 内容
 *
 * 验证点：
 * 1. 启动不卡顿（≤30s 内完成）
 * 2. agent 能成功调用 Skill 并加载 use-artifacts 的 SKILL.md 内容
 * 3. 无 "Unknown skill"、"cache is empty" 等错误
 *
 * 回归检测：2026-07-23 skill 缓存重构后工具调用卡顿问题
 */
import { describe, it, expect, afterEach } from "vitest";
import { launchPeri, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("Skill 工具", () => {
  let tester: TmuxTester;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
  });

  it(
    "加载 builtin skill 返回内容且无卡顿",
    { timeout: 300_000 },
    async () => {
      tester = await launchPeri();

      // 让 agent 调用 Skill 工具加载 builtin skill "use-artifacts"
      await sendPrompt(
        tester,
        '请用 Skill 工具加载 "use-artifacts" skill，然后根据 SKILL.md 中的描述，用中文告诉我这个 skill 的功能。只加载这一个 skill。'
      );

      // 分两阶段等待。最终回复较长时工具行可能滚出视口，因此不能要求工具行
      // 与 footer 同时可见；但必须先因果观察到真实工具完成，再观察 turn 完成。
      await tester.waitFor(
        (screen) => /✓\s+Skill \(use-artifacts\)/.test(screen),
        {
          timeout: 120_000,
          interval: 500,
          message: "等待 use-artifacts Skill 完成",
        },
      );
      await tester.waitFor(
        (screen) => /(?:Brewed for|处理耗时)/.test(screen),
        {
          timeout: 120_000,
          interval: 500,
          message: "等待 Skill 主 turn 完成",
        },
      );

      const capture = await takePeriSnapshot(tester, "skill-tool-use-artifacts");

      expect(capture.text.length).toBeGreaterThan(50);

      // Judge 验证
      const r = await judge({
        ansiRaw: capture.raw,
        criteria: [
          "agent 成功加载了 use-artifacts skill 的内容：回复中应提到 artifact 工具、上传 HTML/Markdown 文件等功能，这些信息来自 SKILL.md 而非 agent 自行编造",
          "不应出现任何 skill 相关的错误信息，如 'Unknown skill'、'cache is empty'、'before_agent may not have run' 等",
          "整体执行速度正常——状态栏显示耗时在合理范围内（≤30s），无长时间卡顿",
        ],
      });
      console.log("Judge:", JSON.stringify(r, null, 2));
      expect(r.pass).toBe(true);
    },
  );
});
