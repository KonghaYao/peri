import { writeFile } from "node:fs/promises";
import path from "node:path";
import { expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { bodyOf, contextModel, latestUser, modelText, resultBlocks, resultText, seekOldest, stored, turn } from "./tui-context-fixture.js";

it("L14 provider usage triggers automatic Full and resumes the same execution before cold restart", { timeout: 180_000 }, async () => {
  const seed = "L14_ORIGINAL_SEED\n" + "L14_ARCHIVED_DETAIL_731 ".repeat(180) + "\nL14_SEED_END";
  const continuation = { owner: "root", execution: "pressure", call: "reason:2", attempt: 1, purpose: "reason" };
  await runReplayScenario({ id: "L14", title: "生产默认阈值下 usage 压力自动 Full，同执行继续并冷重启，原文归档可追溯",
    configure: async (world) => { await writeFile(path.join(world.work, "pressure-ledger.txt"), "L14_TOOL_LEDGER=947\n"); },
    upstream: async () => contextModel((body) => {
      if (body.stream === false || !body.tools?.length) return { text: "L14_AUTO_SUMMARY seed=731 ledger=947; resume the active task and answer." };
      const input = latestUser(body);
      if (input === "L14_SEED") return { text: seed };
      if (input === "L14_COLD_CONTINUE") return { text: "L14_COLD_RESTART_CONTINUED" };
      if (modelText(body).includes("L14_AUTO_SUMMARY")) return { text: "L14_AUTO_SAME_EXECUTION_CONTINUED" };
      return { tool: { id: "l14-pressure-read", name: "Read", input: { file_path: "pressure-ledger.txt" } }, inputTokens: 196_000 };
    }), runOptions: () => ({ gates: [{ identity: continuation, afterBytes: 0 }] }), async run(ctx) {
      let tui = await ctx.startTui("root", { label: "automatic-full", rows: 44 });
      await turn(ctx, tui, "seed", "L14_SEED", "L14_SEED_END");
      ctx.identity.arm(ctx.sessionIds.root!, "pressure"); await tui.prompt("L14_PRESSURE_READ_AND_CONTINUE");
      await ctx.run.waitReached(continuation);
      // The compact note previews the standard continuation preface; the full summary is checked on the model wire and in SQLite.
      const compacted = await tui.waitFor(ctx.checked((s: string) => s.includes("Full compaction completed") && s.includes("Read pressure-ledger.txt")), "automatic Full completes while continuation remains in flight");
      expect(compacted).not.toContain("L14_AUTO_SAME_EXECUTION_CONTINUED");
      expect(compacted).not.toContain("Brewed for"); expect(compacted).toContain("98% ctx");
      expect(compacted.indexOf("Read pressure-ledger.txt")).toBeLessThan(compacted.indexOf("Full compaction completed"));
      expect(ctx.requests).toHaveLength(4);
      const pressure = ctx.requests.slice(1);
      expect(pressure.map((request) => request.context.purpose)).toEqual(["reason", "compact", "reason"]);
      expect(new Set(pressure.map((request) => request.context.execution)).size).toBe(1);
      expect(pressure[0]!.context.execution).not.toBe(ctx.requests[0]!.context.execution);
      expect(new Set(pressure.map((request) => request.context.call)).size).toBe(3);
      const summaryInput = bodyOf(pressure[1]);
      expect(modelText(summaryInput)).toContain("L14_ORIGINAL_SEED");
      expect(modelText(summaryInput)).toContain("L14_PRESSURE_READ_AND_CONTINUE");
      expect(resultText(resultBlocks(summaryInput).find((result) => result.tool_use_id === "l14-pressure-read"))).toContain("L14_TOOL_LEDGER=947");
      const resumed = modelText(bodyOf(pressure[2]));
      expect(resumed).toContain("L14_AUTO_SUMMARY"); expect(resumed).not.toContain("L14_ARCHIVED_DETAIL_731");
      await tui.save("automatic-full-before-continuation"); ctx.run.release(continuation);
      await tui.waitFor(ctx.checked((s: string) => s.includes("L14_AUTO_SAME_EXECUTION_CONTINUED") && s.includes("Brewed for")), "automatic compact does not terminate the active execution");
      await ctx.waitForCalls((snapshot: any) => snapshot.active === 0 && snapshot.calls.length === 4);
      ctx.identity.finish(ctx.sessionIds.root!); await tui.save("automatic-full-continued");
      const beforeRestart = stored(ctx.world, ctx.sessionIds.root!);
      const archived = beforeRestart.find((message) => message.role === "assistant" && message.content.includes("L14_ORIGINAL_SEED"));
      const summary = beforeRestart.find((message) => message.content.includes("L14_AUTO_SUMMARY"));
      expect(archived?.excluded).toBe(1); expect(summary?.excluded).toBe(0);
      expect(JSON.parse(archived!.projection!).superseded_by).toBe(summary!.id);
      expect(beforeRestart.some((message) => message.content.includes("L14_TOOL_LEDGER=947"))).toBe(true);
      await tui.stop();
      tui = await ctx.startTui("root", { label: "after-automatic-full", rows: 44 });
      await tui.waitFor((s) => s.includes("L14_AUTO_SAME_EXECUTION_CONTINUED"), "cold process loads the completed continuation");
      expect(ctx.requests).toHaveLength(4);
      await turn(ctx, tui, "cold-continue", "L14_COLD_CONTINUE", "L14_COLD_RESTART_CONTINUED");
      const cold = modelText(bodyOf(ctx.requests.at(-1)));
      expect(cold).toContain("L14_AUTO_SUMMARY"); expect(cold).toContain("L14_AUTO_SAME_EXECUTION_CONTINUED");
      expect(cold).not.toContain("L14_ARCHIVED_DETAIL_731");
      expect(ctx.requests.at(-1)!.context.execution).not.toBe(pressure[0]!.context.execution);
      const head = await seekOldest(tui, "L14_ORIGINAL_SEED"); expect(head).toContain("L14_SEED");
      await tui.save("archived-history-after-automatic-full");
      const afterRestart = stored(ctx.world, ctx.sessionIds.root!);
      expect(afterRestart.find((message) => message.id === archived!.id)?.excluded).toBe(1);
      ctx.observe("automatic-full", { providerInputTokens: 196_000, productionDefaultContextWindow: 200_000, productionDefaultFullThreshold: 0.95,
        sameExecutionPurposes: pressure.map((request) => request.context.purpose), processStarts: 2, archivedMessageId: archived!.id, summaryMessageId: summary!.id });
    } });
});
