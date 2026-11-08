import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import {
  AgentIdentities, CONFIG_VERSION, assertCanonicalResultOnce, assertFile, assertOsc, backgroundProvider,
  childIdFromResult, completed, configureBackground, configureForcedBackground, countRootUserInputs,
  delegate, expectResult, healthy, identity, options, persistedTrace, read, requestBody, resultText,
  rootCall, text, tool, tools, unexpected, waitCall, write,
} from "./async-agent-fixture.js";

describe("B06–B09 active send, resumed executions, rejected definitions and forced background", () => {
  it("B06 active resume queues a supplement into the same child execution instead of spawning another", async () => {
    let actors: AgentIdentities;
    const child = identity("worker", "reason:1");
    await runReplayScenario({ id: "B06", title: "active child resume 为 Defer 补充消息，同执行且真实工具收到补充", configVersion: CONFIG_VERSION,
      configure: configureBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.call === "reason:1") return tools(delegate("B06-spawn", "B06_INITIAL_WORK", true));
        if (call.owner === "root" && call.call === "reason:2") return tools(tool("B06-send", "Agent", {
          resume_thread_id: childIdFromResult(body, "B06-spawn"), prompt: "B06_SUPPLEMENT_WRITE_SECOND_FILE", run_in_background: false,
        }));
        if (call.owner === "root" && call.call === "reason:3") {
          expectResult(body, "B06-send", "action: send"); expectResult(body, "B06-send", "status: queued");
          expect(childIdFromResult(body, "B06-send")).toBe(childIdFromResult(body, "B06-spawn")); return text("B06_SUPPLEMENT_QUEUED");
        }
        if (call.owner === "worker" && call.call === "reason:1") return tools(write("B06-first-write", "b06-first.txt", "B06_INITIAL_FILE\n"));
        if (call.owner === "worker" && call.call === "reason:2") {
          expectResult(body, "B06-first-write", "b06-first.txt"); expect(JSON.stringify(body.messages)).toContain("B06_SUPPLEMENT_WRITE_SECOND_FILE");
          return tools(write("B06-second-write", "b06-second.txt", "B06_SUPPLEMENT_APPLIED\n"));
        }
        if (call.owner === "worker" && call.call === "reason:3") { expectResult(body, "B06-second-write", "b06-second.txt"); return text("B06_CHILD_ACCEPTED_SUPPLEMENT"); }
        if (call.owner === "root" && call.call === "reason:4") { expect(JSON.stringify(body.messages)).toContain("B06_CHILD_ACCEPTED_SUPPLEMENT"); return tools(read("B06-read", "b06-second.txt")); }
        if (call.owner === "root" && call.call === "reason:5") { expectResult(body, "B06-read", "B06_SUPPLEMENT_APPLIED"); return text("B06_ACTIVE_SEND_CONFIRMED"); }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, [{ tool: "B06-spawn", alias: "worker", background: true }], [child]); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const tui = await ctx.startTui("root", { rows: 60 });
        actors.arm("turn-1"); await tui.prompt("Execute B06.");
        await ctx.run.waitReached(child, { timeout: 30_000 }); await completed(ctx, tui, "B06_SUPPLEMENT_QUEUED");
        await assertOsc(tui, "working", "active-send-child-running");
        expectResult(requestBody(ctx, rootCall("reason:3")), "B06-send", "status: queued");
        ctx.run.release(child); await completed(ctx, tui, "B06_ACTIVE_SEND_CONFIRMED");
        await assertFile(ctx, "b06-first.txt", "B06_INITIAL_FILE\n"); await assertFile(ctx, "b06-second.txt", "B06_SUPPLEMENT_APPLIED\n");
        expect(persistedTrace(ctx).children).toHaveLength(1);
        expect(new Set(actors.seen.filter((call) => call.owner === actors.children.get("worker")).map((call) => call.execution)).size).toBe(1);
        expect(JSON.stringify(requestBody(ctx, identity("worker", "reason:2")).messages).match(/B06_SUPPLEMENT_WRITE_SECOND_FILE/gu)).toHaveLength(1);
        assertCanonicalResultOnce(ctx, "B06_CHILD_ACCEPTED_SUPPLEMENT"); expect(countRootUserInputs(ctx)).toBe(1);
        await assertOsc(tui, "done", "active-send-complete");
        ctx.observe("active-resume-defer", { childThreads: 1, childExecutions: 1, supplementaryDeliveries: 1, files: 2 });
      },
    });
  });

  it("B07 a completed child resumes in a new background task and execution while retaining its thread history", async () => {
    let actors: AgentIdentities;
    const first = identity("worker", "reason:1"), resumed = identity("worker", "reason:1", "resumed");
    await runReplayScenario({ id: "B07", title: "已完成 child 后台恢复，同线程新执行新 task 且历史可读", configVersion: CONFIG_VERSION,
      configure: configureBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:1") return tools(delegate("B07-spawn", "B07_FIRST_RUN", true));
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:2") return text("B07_FIRST_WAITING");
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:3") { expect(JSON.stringify(body.messages)).toContain("B07_FIRST_CHILD_REPORT"); return text("B07_FIRST_RUN_COMPLETE"); }
        if (call.owner === "worker" && call.execution === "created" && call.call === "reason:1") return tools(write("B07-first-write", "b07-first.txt", "B07_FIRST_FILE\n"));
        if (call.owner === "worker" && call.execution === "created" && call.call === "reason:2") return text("B07_FIRST_CHILD_REPORT");
        if (call.owner === "root" && call.execution === "turn-2" && call.call === "reason:1") return tools(tool("B07-resume", "Agent", {
          resume_thread_id: childIdFromResult(body, "B07-spawn"), prompt: "B07_RESUMED_RUN", run_in_background: true,
        }));
        if (call.owner === "root" && call.execution === "turn-2" && call.call === "reason:2") { expectResult(body, "B07-resume", "Background task"); return text("B07_RESUMED_WAITING"); }
        if (call.owner === "worker" && call.execution === "resumed" && call.call === "reason:1") {
          expect(JSON.stringify(body.messages)).toContain("B07_FIRST_CHILD_REPORT"); expect(JSON.stringify(body.messages)).toContain("B07_RESUMED_RUN");
          return tools(read("B07-child-read", "b07-first.txt"));
        }
        if (call.owner === "worker" && call.execution === "resumed" && call.call === "reason:2") { expectResult(body, "B07-child-read", "B07_FIRST_FILE"); return tools(write("B07-resumed-write", "b07-resumed.txt", "B07_HISTORY_REUSED\n")); }
        if (call.owner === "worker" && call.execution === "resumed" && call.call === "reason:3") return text("B07_RESUMED_CHILD_REPORT");
        if (call.owner === "root" && call.execution === "turn-2" && call.call === "reason:3") { expect(JSON.stringify(body.messages)).toContain("B07_RESUMED_CHILD_REPORT"); return tools(read("B07-parent-read", "b07-resumed.txt")); }
        if (call.owner === "root" && call.execution === "turn-2" && call.call === "reason:4") { expectResult(body, "B07-parent-read", "B07_HISTORY_REUSED"); return text("B07_RESUMED_RUN_COMPLETE"); }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, [{ tool: "B07-spawn", alias: "worker", background: true },
        { tool: "B07-resume", alias: "worker", action: "turn-2", execution: "resumed", background: true }], [first, resumed]); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const tui = await ctx.startTui("root", { rows: 65 });
        actors.arm("turn-1"); await tui.prompt("Execute B07 first run.");
        await ctx.run.waitReached(first, { timeout: 30_000 }); await completed(ctx, tui, "B07_FIRST_WAITING");
        ctx.run.release(first); await completed(ctx, tui, "B07_FIRST_RUN_COMPLETE"); await assertOsc(tui, "done", "first-execution-done");
        const thread = actors.children.get("worker");
        actors.arm("turn-2"); await tui.prompt("Execute B07 resumed run.");
        await ctx.run.waitReached(resumed, { timeout: 30_000 }); await completed(ctx, tui, "B07_RESUMED_WAITING");
        await tui.command("tasks"); await tui.waitFor(healthy(ctx, (screen) => screen.includes("B07_RESUMED_RUN")), "resumed task owns new prompt in Tasks panel");
        await tui.save("resumed-task-live"); await tui.key("Escape");
        await assertOsc(tui, "working", "resumed-live-child"); ctx.run.release(resumed);
        await completed(ctx, tui, "B07_RESUMED_RUN_COMPLETE"); await assertOsc(tui, "done", "resumed-child-done");
        expect(actors.children.get("worker")).toBe(thread); expect(persistedTrace(ctx).children).toHaveLength(1);
        expect(new Set(actors.seen.filter((call) => call.owner === thread).map((call) => call.execution)).size).toBe(2);
        const receipt = requestBody(ctx, rootCall("reason:2", "turn-2"));
        const firstTask = resultText(receipt, "B07-spawn").match(/Background task (bg-[0-9a-f-]+)/u)?.[1];
        const nextTask = resultText(receipt, "B07-resume").match(/Background task (bg-[0-9a-f-]+)/u)?.[1];
        expect(firstTask).toBeDefined(); expect(nextTask).toBeDefined(); expect(nextTask).not.toBe(firstTask);
        await assertFile(ctx, "b07-first.txt", "B07_FIRST_FILE\n"); await assertFile(ctx, "b07-resumed.txt", "B07_HISTORY_REUSED\n");
        assertCanonicalResultOnce(ctx, "B07_FIRST_CHILD_REPORT"); assertCanonicalResultOnce(ctx, "B07_RESUMED_CHILD_REPORT");
        expect(countRootUserInputs(ctx)).toBe(2); ctx.observe("resume-history-vs-execution", { childThreads: 1, childExecutions: 2, taskIds: 2, userInputs: 2 });
      },
    });
  });

  it("B08 an invalid agent definition fails before creating a phantom child or background task", async () => {
    let actors: AgentIdentities;
    await runReplayScenario({ id: "B08", title: "无效 Agent 定义明确失败，无幽灵线程或后台任务", configVersion: CONFIG_VERSION,
      configure: configureBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.call === "reason:1") return tools(tool("B08-invalid", "Agent", { subagent_type: "B08-undefined-agent", prompt: "B08_NO_CHILD", run_in_background: true }));
        if (call.owner === "root" && call.call === "reason:2") { expectResult(body, "B08-invalid", "B08-undefined-agent", true); return tools(write("B08-recover", "b08-recovered.txt", "B08_NO_PHANTOM\n")); }
        if (call.owner === "root" && call.call === "reason:3") { expectResult(body, "B08-recover", "b08-recovered.txt"); return text("B08_INVALID_DEFINITION_RECOVERED"); }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, []); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const tui = await ctx.startTui("root", { rows: 55 }); actors.arm("turn-1"); await tui.prompt("Execute B08.");
        await completed(ctx, tui, "B08_INVALID_DEFINITION_RECOVERED");
        expectResult(requestBody(ctx, rootCall("reason:2")), "B08-invalid", "B08-undefined-agent", true);
        expect(resultText(requestBody(ctx, rootCall("reason:2")), "B08-invalid")).not.toContain("Background task bg-");
        expect(persistedTrace(ctx).children).toHaveLength(0); expect(ctx.requests.every((request) => request.context.owner === "root")).toBe(true);
        await assertFile(ctx, "b08-recovered.txt", "B08_NO_PHANTOM\n"); await assertOsc(tui, "done", "invalid-definition-settled");
        await tui.command("tasks");
        await tui.waitFor(healthy(ctx, (screen) => screen.includes("No active tasks") && /Total:\s*0\s*\(0 bg, 0 cron, 0 subagent\)/u.test(screen)), "empty task panel after invalid definition");
        await tui.save("no-phantom-task"); await tui.key("Escape");
        ctx.observe("rejected-definition", { childThreads: 0, childModelCalls: 0, backgroundReceipt: false, parentRecovered: true });
      },
    });
  });

  it("B09 full-async-tools forces both omitted and explicit false Agent calls into background", async () => {
    let actors: AgentIdentities;
    const omitted = identity("omitted", "reason:1"), explicit = identity("explicit", "reason:1"), parent = rootCall("reason:2");
    await runReplayScenario({ id: "B09", title: "full-async-tools 对 Agent 缺省及显式 false 均强制后台", configVersion: CONFIG_VERSION,
      configure: configureForcedBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.call === "reason:1") return tools(
          tool("B09-omitted", "Agent", { subagent_type: "replay-worker", prompt: "B09_OMITTED_BACKGROUND" }),
          delegate("B09-explicit", "B09_FALSE_BACKGROUND", false));
        if (["omitted", "explicit"].includes(call.owner) && call.call === "reason:1") return tools(write(`B09-write-${call.owner}`, `b09-${call.owner}.txt`, `B09_${call.owner}_FILE\n`));
        if (["omitted", "explicit"].includes(call.owner) && call.call === "reason:2") return text(`B09_${call.owner}_REPORT`);
        if (call.owner === "root" && call.call === "reason:2") {
          expectResult(body, "B09-omitted", "Background task"); expectResult(body, "B09-explicit", "Background task");
          return tools(read("B09-read-omitted", "b09-omitted.txt"), read("B09-read-explicit", "b09-explicit.txt"));
        }
        if (call.owner === "root" && call.call === "reason:3") {
          for (const alias of ["omitted", "explicit"]) { expectResult(body, `B09-read-${alias}`, `B09_${alias}_FILE`); expect(JSON.stringify(body.messages)).toContain(`B09_${alias}_REPORT`); }
          return text("B09_FORCED_BACKGROUND_CONFIRMED");
        }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, [{ tool: "B09-omitted", alias: "omitted", background: true },
        { tool: "B09-explicit", alias: "explicit", background: true }], [omitted, explicit, parent]); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const tui = await ctx.startTui("root", { rows: 60 }); actors.arm("turn-1"); await tui.prompt("Execute B09.");
        await ctx.run.waitReached(omitted, { timeout: 30_000 }); await ctx.run.waitReached(explicit, { timeout: 30_000 }); await ctx.run.waitReached(parent, { timeout: 30_000 });
        const receipts = requestBody(ctx, parent); expectResult(receipts, "B09-omitted", "Background task"); expectResult(receipts, "B09-explicit", "Background task");
        ctx.run.release(omitted); await waitCall(ctx, identity("omitted", "reason:2"));
        await tui.waitFor(healthy(ctx, (screen) => /✔\s+agent\s+B09_OMITTED_BACKGROUND/u.test(screen)), "omitted child terminal delivery precedes explicit-false child delivery");
        ctx.run.release(explicit); await waitCall(ctx, identity("explicit", "reason:2"));
        await tui.waitFor(healthy(ctx, (screen) => /✔\s+agent\s+B09_OMITTED_BACKGROUND/u.test(screen)
          && /✔\s+agent\s+B09_FALSE_BACKGROUND/u.test(screen)), "both forced background children reached terminal task rows");
        await tui.save("forced-background-terminal-rows");
        ctx.run.release(parent); await completed(ctx, tui, "B09_FORCED_BACKGROUND_CONFIRMED");
        for (const alias of ["omitted", "explicit"]) { await assertFile(ctx, `b09-${alias}.txt`, `B09_${alias}_FILE\n`); assertCanonicalResultOnce(ctx, `B09_${alias}_REPORT`); }
        expect(persistedTrace(ctx).children).toHaveLength(2); await assertOsc(tui, "done", "forced-background-done");
        ctx.observe("beta-agent-contract", { omitted: "background", explicitFalse: "background", childThreads: 2 });
      },
    });
  });
});
