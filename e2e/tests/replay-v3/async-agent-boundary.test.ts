import { performance } from "node:perf_hooks";
import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { startBackgroundAcp } from "./async-agent-acp.js";
import {
  AgentIdentities, CONFIG_VERSION, assertCanonicalResultOnce, assertFile, assertMissingFile, assertOsc,
  backgroundProvider, completed, configureBackground, countRootUserInputs, delegate, expectResult, healthy,
  identity, options, persistedTrace, read, requestBody, rootCall, rootPayloads, text, tools, unexpected, waitCall, write,
} from "./async-agent-fixture.js";

describe("B10–B12 actual TurnDone, continuation cancellation and process restart boundaries", () => {
  it("B10 [slow] child completion after the actual 120 second handoff TurnDone starts a fresh automatic execution", { timeout: 240_000 }, async () => {
    let actors: AgentIdentities;
    const child = identity("worker", "reason:1");
    await runReplayScenario({ id: "B10", title: "真实 120 秒 bounded handoff TurnDone 后 child 回调无需用户新输入", configVersion: CONFIG_VERSION, requestTimeoutMs: 180_000,
      configure: configureBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:1") return tools(delegate("B10-spawn", "B10_WAIT_BEYOND_PARENT_TURN", true));
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:2") return text("B10_PARENT_WAITING_FOR_HANDOFF");
        if (call.owner === "worker" && call.call === "reason:1") return tools(write("B10-write", "b10-after-turn.txt", "B10_AFTER_TURN_DONE\n"));
        if (call.owner === "worker" && call.call === "reason:2") return text("B10_CHILD_FINISHED_AFTER_TURN_DONE");
        if (call.owner === "root" && call.execution === "continuation" && call.call === "reason:1") {
          expect(JSON.stringify(body.messages)).toContain("B10_CHILD_FINISHED_AFTER_TURN_DONE"); return tools(read("B10-read", "b10-after-turn.txt"));
        }
        if (call.owner === "root" && call.execution === "continuation" && call.call === "reason:2") { expectResult(body, "B10-read", "B10_AFTER_TURN_DONE"); return text("B10_LATE_RESULT_AUTO_PROCESSED"); }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, [{ tool: "B10-spawn", alias: "worker", background: true }], [child]); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const acp = await startBackgroundAcp(ctx, actors);
        let primaryFailure: unknown;
        try {
          const first = await acp.enqueue("turn-1", "Execute B10 without any second user prompt.", "00000000-0000-0000-0000-000000001001");
          await ctx.run.waitReached(child, { timeout: 30_000 }); await waitCall(ctx, rootCall("reason:2"));
          const waitingAt = performance.now();
          // This is the real production deadline. TurnSuspended and a UI timer cannot satisfy it.
          await acp.done(first.requestId, "end_turn", 150_000);
          const boundedWaitMs = performance.now() - waitingAt;
          expect(boundedWaitMs).toBeGreaterThan(115_000); await acp.idle();
          expect(rootPayloads(ctx).filter((payload) => payload.type !== "message" && JSON.stringify(payload).includes("pending_handoff"))).toHaveLength(1);
          await assertMissingFile(ctx, "b10-after-turn.txt");
          expect(ctx.proxy.snapshot(ctx.runId).calls.find((call: any) => call.identity.owner === "worker")?.terminal).toBeNull();
          actors.arm("continuation"); ctx.run.release(child);
          await waitCall(ctx, rootCall("reason:2", "continuation"));
          await expect.poll(() => { acp.healthy(); return JSON.stringify(persistedTrace(ctx).messages); }, { timeout: 30_000 }).toContain("B10_LATE_RESULT_AUTO_PROCESSED");
          await acp.idle(); await assertFile(ctx, "b10-after-turn.txt", "B10_AFTER_TURN_DONE\n");
          expectResult(requestBody(ctx, rootCall("reason:2", "continuation")), "B10-read", "B10_AFTER_TURN_DONE");
          assertCanonicalResultOnce(ctx, "B10_CHILD_FINISHED_AFTER_TURN_DONE"); expect(countRootUserInputs(ctx)).toBe(1);
          expect(new Set(actors.seen.filter((call) => call.owner === ctx.sessionIds.root).map((call) => call.execution)).size).toBe(2);
          ctx.observe("actual-turn-done-before-child", { boundedWaitMs, originalRequestId: first.requestId, userInputs: 1, rootExecutions: 2 });
        } catch (error) { primaryFailure = error; throw error; }
        finally { try { await acp.close(); } catch (error) { if (!primaryFailure) throw error; ctx.observe("acp-cleanup-error", String(error)); } }
        const tui = await ctx.startTui("root", { rows: 65 });
        await tui.waitFor(healthy(ctx, (screen) => screen.includes("B10_LATE_RESULT_AUTO_PROCESSED")), "ACP late result restored in real TUI history");
        await assertOsc(tui, "idle", "historical-completion-is-not-live"); await tui.save("late-result-history");
      },
    });
  });

  it("B11 cancelling the automatic continuation suppresses later automatic wakes until explicit user input", async () => {
    let actors: AgentIdentities;
    const first = identity("first", "reason:1"), second = identity("second", "reason:1"), parent = rootCall("reason:2"), continuation = rootCall("reason:1", "continuation");
    await runReplayScenario({ id: "B11", title: "停止自动续跑后晚到 child 不复活父回合，显式输入后只消费一次", configVersion: CONFIG_VERSION,
      configure: configureBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:1") return tools(delegate("B11-first", "B11_FIRST_WORK", true), delegate("B11-second", "B11_SECOND_WORK", true));
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:2") return text("B11_FORBIDDEN_CANCELLED_PARENT_TAIL");
        if (["first", "second"].includes(call.owner) && call.call === "reason:1") return tools(write(`B11-write-${call.owner}`, `b11-${call.owner}.txt`, `B11_${call.owner}_FILE\n`));
        if (["first", "second"].includes(call.owner) && call.call === "reason:2") return text(`B11_${call.owner}_REPORT`);
        if (call.owner === "root" && call.execution === "continuation" && call.call === "reason:1") { expect(JSON.stringify(body.messages)).toContain("B11_first_REPORT"); return text("B11_FORBIDDEN_CANCELLED_CONTINUATION_TAIL"); }
        if (call.owner === "root" && call.execution === "turn-2" && call.call === "reason:1") {
          expect(JSON.stringify(body.messages)).toContain("B11_second_REPORT"); return tools(read("B11-read-first", "b11-first.txt"), read("B11-read-second", "b11-second.txt"));
        }
        if (call.owner === "root" && call.execution === "turn-2" && call.call === "reason:2") {
          expectResult(body, "B11-read-first", "B11_first_FILE"); expectResult(body, "B11-read-second", "B11_second_FILE"); return text("B11_EXPLICIT_INPUT_UNPAUSED_RESULTS");
        }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, [{ tool: "B11-first", alias: "first", background: true }, { tool: "B11-second", alias: "second", background: true }],
        [first, second, parent, continuation]); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const acp = await startBackgroundAcp(ctx, actors);
        let primaryFailure: unknown;
        try {
          const initial = await acp.enqueue("turn-1", "Execute B11 first turn.", "00000000-0000-0000-0000-000000001101");
          for (const gate of [first, second, parent]) await ctx.run.waitReached(gate, { timeout: 30_000 });
          await acp.cancel(initial.requestId); await waitCall(ctx, parent, "client_abort");
          actors.arm("continuation"); ctx.run.release(first); await ctx.run.waitReached(continuation, { timeout: 30_000 });
          await acp.cancel(); await waitCall(ctx, continuation, "client_abort");
          ctx.run.release(second); await acp.completedTask("B11_second_REPORT");
          await acp.idle();
          // Ordered ACP snapshots run after the child-completed event: this is a host barrier, not a sleep-based negative assertion.
          for (let barrier = 0; barrier < 3; barrier++) {
            expect((await acp.snapshot()).activeRequestId).toBeUndefined();
            expect(acp.activeExecutions()).toEqual([]); expect(acp.executions()).toHaveLength(2);
          }
          expect(new Set(ctx.requests.filter((request) => request.context.owner === "root").map((request) => request.context.execution))).toEqual(new Set(["turn-1", "continuation"]));
          expect(JSON.stringify(acp.notifications)).not.toContain("B11_FORBIDDEN_CANCELLED_CONTINUATION_TAIL");
          const manual = await acp.enqueue("turn-2", "Execute B11 explicitly resume after Stop.", "00000000-0000-0000-0000-000000001102");
          await acp.done(manual.requestId); await acp.idle();
          expect(acp.executions()).toHaveLength(3);
          for (const alias of ["first", "second"]) { await assertFile(ctx, `b11-${alias}.txt`, `B11_${alias}_FILE\n`); assertCanonicalResultOnce(ctx, `B11_${alias}_REPORT`); }
          expect(countRootUserInputs(ctx)).toBe(2);
          expect(new Set(ctx.requests.filter((request) => request.context.owner === "root").map((request) => request.context.execution))).toEqual(new Set(["turn-1", "continuation", "turn-2"]));
          ctx.observe("stop-continuation-is-sticky", { suppressedWakes: 1, rootExecutions: 3, userInputs: 2, orderedHostBarriers: 3 });
        } catch (error) { primaryFailure = error; throw error; }
        finally { try { await acp.close(); } catch (error) { if (!primaryFailure) throw error; ctx.observe("acp-cleanup-error", String(error)); } }
        const tui = await ctx.startTui("root", { rows: 65 });
        await tui.waitFor(healthy(ctx, (screen) => screen.includes("B11_EXPLICIT_INPUT_UNPAUSED_RESULTS")), "explicitly resumed result persisted in real TUI");
        expect(await tui.screen()).not.toContain("B11_FORBIDDEN_CANCELLED_CONTINUATION_TAIL");
        expect(JSON.stringify(rootPayloads(ctx))).not.toContain("B11_FORBIDDEN_CANCELLED_CONTINUATION_TAIL");
        expect(JSON.stringify(rootPayloads(ctx))).not.toContain("B11_FORBIDDEN_CANCELLED_PARENT_TAIL");
        await assertOsc(tui, "idle", "restart-after-explicit-resume");
      },
    });
  });

  it("B12 restarting the process restores completed child history without resurrecting execution or replaying tools", async () => {
    let actors: AgentIdentities;
    const child = identity("worker", "reason:1");
    await runReplayScenario({ id: "B12", title: "进程重启恢复 child 历史，不恢复旧执行、工具副作用或 working 状态", configVersion: CONFIG_VERSION,
      configure: configureBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:1") return tools(delegate("B12-spawn", "B12_COMPLETED_CHILD_HISTORY", true));
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:2") return text("B12_PARENT_WAITING");
        if (call.owner === "worker" && call.call === "reason:1") return tools(write("B12-write", "b12-once.txt", "B12_CHILD_WROTE_ONCE\n"));
        if (call.owner === "worker" && call.call === "reason:2") return text("B12_CHILD_HISTORY_REPORT");
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:3") { expect(JSON.stringify(body.messages)).toContain("B12_CHILD_HISTORY_REPORT"); return text("B12_FIRST_PROCESS_COMPLETE"); }
        if (call.owner === "root" && call.execution === "turn-2" && call.call === "reason:1") { expect(JSON.stringify(body.messages)).toContain("B12_FIRST_PROCESS_COMPLETE"); return tools(read("B12-after-restart-read", "b12-once.txt")); }
        if (call.owner === "root" && call.execution === "turn-2" && call.call === "reason:2") { expectResult(body, "B12-after-restart-read", "B12_CHILD_WROTE_ONCE"); return text("B12_RESTART_HISTORY_VERIFIED"); }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, [{ tool: "B12-spawn", alias: "worker", background: true }], [child]); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const first = await ctx.startTui("root", { rows: 60 }); actors.arm("turn-1"); await first.prompt("Execute B12 first process.");
        await ctx.run.waitReached(child, { timeout: 30_000 }); await completed(ctx, first, "B12_PARENT_WAITING");
        ctx.run.release(child); await completed(ctx, first, "B12_FIRST_PROCESS_COMPLETE"); await assertOsc(first, "done", "first-process-done"); await first.stop();
        const before = ctx.requests.length, second = await ctx.startTui("root", { rows: 65 });
        await second.waitFor(healthy(ctx, (screen) => screen.includes("B12_FIRST_PROCESS_COMPLETE")), "completed parent and child history in restarted PTY");
        await assertOsc(second, "idle", "historical-child-is-not-working"); await second.command("tasks");
        await second.waitFor(healthy(ctx, (screen) => screen.includes("Total:")), "restarted Tasks panel is loaded");
        expect(await second.screen()).not.toContain("[running]"); expect(await second.screen()).not.toContain("[reconnecting]");
        await second.save("no-resurrected-background-task"); await second.key("Escape");
        await second.waitFor(healthy(ctx, (screen) => !screen.includes("─ Tasks ─")), "Tasks panel is actually closed before further input", 10_000);
        expect(ctx.requests.length).toBe(before); await assertFile(ctx, "b12-once.txt", "B12_CHILD_WROTE_ONCE\n");
        actors.arm("turn-2"); await second.prompt("Execute B12 verify stored history."); await completed(ctx, second, "B12_RESTART_HISTORY_VERIFIED");
        await assertOsc(second, "done", "new-explicit-turn-done"); expect(persistedTrace(ctx).children).toHaveLength(1);
        const childWrites = persistedTrace(ctx).messages.filter((entry) => entry.thread === actors.children.get("worker"))
          .flatMap((entry) => entry.message.tool_calls ?? []).filter((call: any) => call.name === "Write");
        expect(childWrites).toHaveLength(1); expect(ctx.requests.filter((request) => request.context.owner === "worker")).toHaveLength(2);
        assertCanonicalResultOnce(ctx, "B12_CHILD_HISTORY_REPORT"); expect(countRootUserInputs(ctx)).toBe(2);
        ctx.observe("history-is-not-live-execution", { processes: 2, childExecutions: 1, childWrites: 1, startupModelCalls: 0 });
      },
    });
  });
});
