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
import { readFile, rm } from "node:fs/promises";
import path from "node:path";
import { PROJECT_ROOT, sendPrompt, takePeriSnapshot } from "../../helpers/peri.js";
import {
  launchReplayTui,
  makeReplayHome,
  replayDebug,
  startReplayModelServer,
  type ReplayServer,
} from "../../helpers/replay-model.js";
import { judge } from "../../helpers/judge.js";
import type { TmuxTester } from "tui-tester";

describe("Skill 工具", () => {
  let tester: TmuxTester;
  let replay: ReplayServer;
  let home: string;

  afterEach(async () => {
    if (tester?.isRunning()) {
      await tester.stop().catch(() => {});
    }
    if (replay) await replay.close();
    if (home) await rm(home, { recursive: true, force: true });
  });

  it(
    "加载 builtin skill 返回内容且无卡顿",
    { timeout: 300_000 },
    async () => {
      const skillSource = await readFile(path.join(
        PROJECT_ROOT, "mcp-packages/workspace/src/resources/builtin/skills/use-artifacts/SKILL.md",
      ), "utf8");
      const skillBody = skillSource.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n/, "").trim();
      replay = await startReplayModelServer([
        {
          prompt: "E2E_SKILL_LOAD",
          toolUses: [{ name: "SkillTool", input: { skill_name: "use-artifacts" } }],
        },
        { text: "use-artifacts 加载成功：用 artifact 工具上传 HTML 或 Markdown 文件，生成可分享的稳定 URL，用于进度、报告和最终成果。" },
      ]);
      home = await makeReplayHome(replay.port);
      tester = await launchReplayTui({ home });
      const startedAt = Date.now();

      // 让 agent 调用 Skill 工具加载 builtin skill "use-artifacts"
      await sendPrompt(
        tester,
        'E2E_SKILL_LOAD：请用 Skill 工具加载 "use-artifacts"，根据实际 SKILL.md 用中文简述功能。只加载这一个 skill。'
      );

      // 分两阶段等待。最终回复较长时工具行可能滚出视口，因此不能要求工具行
      // 与 footer 同时可见；但必须先因果观察到真实工具完成，再观察 turn 完成。
      await tester.waitFor(
        (screen) => /✓\s+Skill \(use-artifacts\)/.test(screen),
        {
          timeout: 30_000,
          interval: 500,
          message: "等待 use-artifacts Skill 完成",
        },
      );
      await tester.waitFor(
        (screen) => /(?:Brewed for|处理耗时)/.test(screen),
        {
          timeout: 30_000,
          interval: 500,
          message: "等待 Skill 主 turn 完成",
        },
      );

      const capture = await takePeriSnapshot(tester, "skill-tool-use-artifacts");

      expect(Date.now() - startedAt, "真实 Skill 调用与主 turn 完成不得超过 30s").toBeLessThanOrEqual(30_000);
      expect(capture.text.length).toBeGreaterThan(50);
      expect(capture.text).toContain("artifact");
      expect(capture.text).toMatch(/HTML.*Markdown/);
      expect(capture.text).not.toMatch(/Unknown skill|cache is empty|before_agent may not have run/i);
      const duration = /(?:Brewed for|处理耗时)\s+(?:(\d+)m\s+)?(\d+)s/.exec(capture.text);
      expect(duration, "完成态必须显示真实耗时").not.toBeNull();
      expect(Number(duration![1] ?? 0) * 60 + Number(duration![2])).toBeLessThanOrEqual(30);
      const mainRequests = replay.requests.filter((request) => request.isMain);
      expect(mainRequests, replayDebug(replay)).toHaveLength(2);
      expect(mainRequests[0].body.tools.some((tool: any) => tool.name === "SkillTool")).toBe(true);
      const results = mainRequests[1].body.messages.flatMap((message: any) =>
        Array.isArray(message.content) ? message.content.filter((block: any) =>
          block.type === "tool_result" && block.tool_use_id === "replay-1-0",
        ) : [],
      );
      expect(results, "模型第二次请求必须收到真实 SkillTool 的匹配回执").toHaveLength(1);
      expect(results[0].is_error).not.toBe(true);
      const resultText = typeof results[0].content === "string" ? results[0].content
        : results[0].content.map((block: any) => block.text ?? "").join("\n");
      expect(resultText).toContain(skillBody);
      expect(replay.misses(), replayDebug(replay)).toBe(0);

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
