import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario, type ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import { assertToolResult, startToolsFixture, toolsPlan, type ToolCaseId } from "./tools-fixture.js";

const cases: Array<{ id: ToolCaseId; title: string; file?: string; content?: string; visibleTool: RegExp }> = [
  { id: "T01", title: "Write 后实际 Read 回同一文件", file: "note.txt", content: "T01_WRITTEN_AND_READ\n", visibleTool: /Read/ },
  { id: "T02", title: "Read 后 Edit 精确替换并再次读取", file: "config.txt", content: "mode=new\nkeep=stable\n", visibleTool: /Edit/ },
  { id: "T03", title: "Bash 的标准输出和真实文件副作用", file: "command-result.txt", content: "T03_FILE_EFFECT\n", visibleTool: /Shell/ },
  { id: "T04", title: "Read 失败后 Write 修复再成功 Read", file: "missing.txt", content: "T04_REPAIRED_CONTENT\n", visibleTool: /Read/ },
  { id: "T05", title: "发现 MCP 资源工具并读取真实 workspace 资源", visibleTool: /mcp_read_resource|ExecuteExtraTool/ },
  { id: "T06", title: "Prompt hook 的真实模型判定改写 Bash 输入", file: "hook-result.txt", content: "T06_HOOK_APPROVED\n", visibleTool: /Shell/ },
];

async function assertBusiness(ctx: ReplayScenarioContext, business: typeof cases[number]): Promise<void> {
  const plan = toolsPlan(business.id, ctx.world);
  const sessionId = ctx.sessionIds.root!;
  if (business.id === "T04") {
    await expect(access(path.join(ctx.world.work, "missing.txt"))).rejects.toMatchObject({ code: "ENOENT" });
  }
  const tui = await ctx.startTui("root");
  ctx.identity.arm(sessionId, "business-turn");
  await tui.prompt(`${business.id}_BUSINESS_CASE`);
  const screen = await tui.waitFor(ctx.checked((value: string) => value.includes(`${business.id}_BUSINESS_DONE`) && value.includes("Brewed for")), "tool result and completed business turn", 60_000);
  expect(screen).toMatch(business.visibleTool);
  const expectedCalls = plan.length + 1 + Number(business.id === "T06");
  await ctx.waitForCalls((state) => state.active === 0 && state.calls.length === expectedCalls, 15_000);
  ctx.identity.finish(sessionId);
  const main = ctx.requests.filter((request) => request.context.purpose === "reason");
  expect(main).toHaveLength(plan.length + 1);
  const lastBody = JSON.parse(main.at(-1)!.request.body.toString("utf8"));
  const results = plan.map((step) => assertToolResult(lastBody, step.id, step.result));
  if (business.id === "T04") {
    expect(results.map((result) => Boolean(result.is_error))).toEqual([true, false, false]);
  }
  const hooks = ctx.requests.filter((request) => request.context.purpose === "hook");
  if (business.id === "T06") {
    expect(hooks).toHaveLength(1);
    expect(hooks[0]!.context.execution).toBe(main[0]!.context.execution);
    expect(hooks[0]!.context.owner).toBe(main[0]!.context.owner);
    expect(hooks[0]!.context.call).toContain("PreToolUse:reason:1:t06-hooked-bash:");
    expect(JSON.parse(hooks[0]!.request.body.toString("utf8")).system).toBeDefined();
  } else expect(hooks).toHaveLength(0);
  if (business.file) {
    const content = await readFile(path.join(ctx.world.work, business.file), "utf8");
    expect(content).toBe(business.content);
    ctx.observe("actual-file", { name: business.file, content });
  }
  ctx.observe("tool-results", results.map((result) => ({ id: result.tool_use_id, isError: Boolean(result.is_error), content: result.content })));
  ctx.observe("model-calls", { reason: main.length, hook: hooks.length });
  await tui.save("business-complete");
}

describe("T01–T06 真实工具业务录制和离线回放", () => {
  for (const business of cases) {
    it(`${business.id} ${business.title}`, { timeout: 180_000 }, async () => {
      await runReplayScenario({
        id: business.id,
        title: business.title,
        async configure(world) {
          if (business.id === "T02") await writeFile(path.join(world.work, "config.txt"), "mode=old\nkeep=stable\n");
          if (business.id === "T06") {
            const claude = path.join(world.home, ".claude");
            await mkdir(claude, { recursive: true });
            await writeFile(path.join(claude, "settings.json"), JSON.stringify({ hooks: {
              PreToolUse: [{ matcher: "Bash", hooks: [{ type: "prompt", prompt: "T06_HOOK_EVALUATION: Return a structured PreToolUse decision with updatedInput.", timeout: 15 }] }],
            } }));
          }
        },
        upstream: (world) => startToolsFixture(business.id, world),
        run: (context) => assertBusiness(context, business),
      });
    });
  }
});
