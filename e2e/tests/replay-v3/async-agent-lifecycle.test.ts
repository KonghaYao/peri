import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import {
  AgentIdentities, CONFIG_VERSION, assertCanonicalResultOnce, assertChildTrace, assertFile, assertMissingFile,
  assertOsc, backgroundProvider, completed, configureBackground, countRootUserInputs, delegate, expectResult,
  healthy, identity, options, oscStates, persistedTrace, read, requestBody, rootCall, rootPayloads, text, threadState,
  tools, unexpected, waitCall, write,
} from "./async-agent-fixture.js";

describe("B01–B05 async Agent completion, independent cancellation and real Tasks UI", () => {
  it("B01 suspended parent stays working and consumes one child result without another user prompt", async () => {
    let actors: AgentIdentities;
    const child = identity("worker", "reason:1");
    await runReplayScenario({ id: "B01", title: "父挂起保持 working，异步结果自动处理且持久化一次", configVersion: CONFIG_VERSION,
      configure: configureBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.call === "reason:1") return tools(delegate("B01-spawn", "B01_BACKGROUND_WORK", true));
        if (call.owner === "root" && call.call === "reason:2") { expectResult(body, "B01-spawn", "Background task"); return text("B01_PARENT_WAITING"); }
        if (call.owner === "worker" && call.call === "reason:1") return tools(write("B01-write", "b01.txt", "B01_CHILD_FILE\n"));
        if (call.owner === "worker" && call.call === "reason:2") { expectResult(body, "B01-write", "b01.txt"); return text("B01_CHILD_REPORT"); }
        if (call.owner === "root" && call.call === "reason:3") { expect(JSON.stringify(body.messages)).toContain("B01_CHILD_REPORT"); return tools(read("B01-read", "b01.txt")); }
        if (call.owner === "root" && call.call === "reason:4") { expectResult(body, "B01-read", "B01_CHILD_FILE"); return text("B01_PARENT_FINISHED"); }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, [{ tool: "B01-spawn", alias: "worker", background: true }], [child]); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const tui = await ctx.startTui("root", { rows: 55 });
        actors.arm("turn-1"); await tui.prompt("Execute B01.");
        await ctx.run.waitReached(child, { timeout: 30_000 });
        await completed(ctx, tui, "B01_PARENT_WAITING");
        await assertOsc(tui, "working", "suspended-parent-still-working");
        await assertMissingFile(ctx, "b01.txt");
        ctx.run.release(child);
        await completed(ctx, tui, "B01_PARENT_FINISHED");
        await assertOsc(tui, "done", "last-child-done");
        await assertFile(ctx, "b01.txt", "B01_CHILD_FILE\n");
        assertChildTrace(ctx, actors, "worker", "B01-spawn", "b01.txt", "B01_CHILD_REPORT");
        assertCanonicalResultOnce(ctx, "B01_CHILD_REPORT");
        expectResult(requestBody(ctx, rootCall("reason:4")), "B01-read", "B01_CHILD_FILE");
        expect(countRootUserInputs(ctx)).toBe(1);
        ctx.observe("async-parent-suspend-wake", { userInputs: 1, canonicalResults: 1, states: await oscStates(tui, "state-sequence") });
      },
    });
  });

  it("B02 two async children finish right then left with separate files, results and live states", async () => {
    let actors: AgentIdentities;
    const left = identity("left", "reason:1"), right = identity("right", "reason:1");
    await runReplayScenario({ id: "B02", title: "双异步 child 逆序完成，结果归属与 working 终态准确", configVersion: CONFIG_VERSION,
      configure: configureBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.call === "reason:1") return tools(delegate("B02-left", "B02_LEFT_WORK", true), delegate("B02-right", "B02_RIGHT_WORK", true));
        if (call.owner === "root" && call.call === "reason:2") return text("B02_PARENT_WAITING");
        if (["left", "right"].includes(call.owner) && call.call === "reason:1") return tools(write(`B02-write-${call.owner}`, `b02-${call.owner}.txt`, `B02_${call.owner.toUpperCase()}_FILE\n`));
        if (["left", "right"].includes(call.owner) && call.call === "reason:2") return text(`B02_${call.owner.toUpperCase()}_REPORT`);
        if (call.owner === "root" && call.call === "reason:3") { expect(JSON.stringify(body.messages)).toContain("B02_RIGHT_REPORT"); expect(JSON.stringify(body.messages)).not.toContain("B02_LEFT_REPORT"); return tools(read("B02-read-right", "b02-right.txt")); }
        if (call.owner === "root" && call.call === "reason:4") { expectResult(body, "B02-read-right", "B02_RIGHT_FILE"); return text("B02_RIGHT_PROCESSED_LEFT_PENDING"); }
        if (call.owner === "root" && call.call === "reason:5") { expect(JSON.stringify(body.messages)).toContain("B02_LEFT_REPORT"); return tools(read("B02-read-left", "b02-left.txt")); }
        if (call.owner === "root" && call.call === "reason:6") { expectResult(body, "B02-read-left", "B02_LEFT_FILE"); return text("B02_BOTH_RESULTS_PROCESSED"); }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, [{ tool: "B02-left", alias: "left", background: true }, { tool: "B02-right", alias: "right", background: true }], [left, right]); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const tui = await ctx.startTui("root", { cols: 140, rows: 65 });
        actors.arm("turn-1"); await tui.prompt("Execute B02.");
        await ctx.run.waitReached(left, { timeout: 30_000 }); await ctx.run.waitReached(right, { timeout: 30_000 });
        await completed(ctx, tui, "B02_PARENT_WAITING");
        await assertOsc(tui, "working", "both-running");
        ctx.run.release(right);
        await completed(ctx, tui, "B02_RIGHT_PROCESSED_LEFT_PENDING");
        await assertOsc(tui, "working", "right-done-left-running");
        await assertMissingFile(ctx, "b02-left.txt");
        await assertFile(ctx, "b02-right.txt", "B02_RIGHT_FILE\n");
        expect(ctx.proxy.snapshot(ctx.runId).calls.find((call: any) => call.identity.owner === "left")?.terminal).toBeNull();
        const lines = (await tui.screen()).split("\n");
        const row = lines.findIndex((line) => /✔\s+agent\s+B02_RIGHT_WORK/u.test(line));
        expect(row, "completed right task has a real clickable bottom row").toBeGreaterThanOrEqual(0);
        const column = lines[row]!.indexOf("B02_RIGHT_WORK") + 1;
        await tui.type(`\x1b[<0;${column};${row + 1}M\x1b[<0;${column};${row + 1}m`);
        const detail = await tui.waitFor(healthy(ctx, (screen) => screen.includes("SubAgent Detail")
          && screen.slice(screen.indexOf("SubAgent Detail")).includes("B02_RIGHT_REPORT")), "clicked right task owns the right child's nested transcript");
        const panel = detail.slice(detail.indexOf("SubAgent Detail"));
        expect(panel).toContain("b02-right.txt"); expect(panel).not.toContain("b02-left.txt");
        await tui.save("right-task-detail-keeps-exact-child-owner"); await tui.key("Escape");
        await tui.waitFor(healthy(ctx, (screen) => !screen.includes("SubAgent Detail")), "task detail closes before releasing left child");
        ctx.run.release(left);
        await completed(ctx, tui, "B02_BOTH_RESULTS_PROCESSED");
        await assertOsc(tui, "done", "both-done");
        for (const alias of ["left", "right"]) {
          await assertFile(ctx, `b02-${alias}.txt`, `B02_${alias.toUpperCase()}_FILE\n`);
          assertChildTrace(ctx, actors, alias, `B02-${alias}`, `b02-${alias}.txt`, `B02_${alias.toUpperCase()}_REPORT`);
          assertCanonicalResultOnce(ctx, `B02_${alias.toUpperCase()}_REPORT`);
        }
        const earlier = JSON.stringify(requestBody(ctx, rootCall("reason:3")).messages);
        expect(earlier).toContain("B02_RIGHT_REPORT"); expect(earlier).not.toContain("B02_LEFT_REPORT");
        expect(persistedTrace(ctx).children).toHaveLength(2);
        expect(countRootUserInputs(ctx)).toBe(1);
        ctx.observe("reverse-async-completion", { completionOrder: ["right", "left"], children: 2, userInputs: 1 });
      },
    });
  });

  it("B03 parent Ctrl+C preserves an independent child and permits its single automatic continuation", async () => {
    let actors: AgentIdentities;
    const child = identity("worker", "reason:1"), parent = rootCall("reason:2");
    await runReplayScenario({ id: "B03", title: "取消普通父回合不杀 Independent child，结果一次自动续跑", configVersion: CONFIG_VERSION,
      configure: configureBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:1") return tools(delegate("B03-spawn", "B03_INDEPENDENT_WORK", true));
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:2") return text("B03_CANCELLED_PARENT_FORBIDDEN_TAIL");
        if (call.owner === "worker" && call.call === "reason:1") return tools(write("B03-write", "b03.txt", "B03_SURVIVED_PARENT_CANCEL\n"));
        if (call.owner === "worker" && call.call === "reason:2") return text("B03_CHILD_COMPLETED_AFTER_PARENT_CANCEL");
        if (call.owner === "root" && call.execution === "continuation" && call.call === "reason:1") { expect(JSON.stringify(body.messages)).toContain("B03_CHILD_COMPLETED_AFTER_PARENT_CANCEL"); return tools(read("B03-read", "b03.txt")); }
        if (call.owner === "root" && call.execution === "continuation" && call.call === "reason:2") { expectResult(body, "B03-read", "B03_SURVIVED_PARENT_CANCEL"); return text("B03_INDEPENDENT_RESULT_PROCESSED"); }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, [{ tool: "B03-spawn", alias: "worker", background: true }], [child, parent]); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const tui = await ctx.startTui("root", { rows: 55 });
        actors.arm("turn-1"); await tui.prompt("Execute B03.");
        await ctx.run.waitReached(child, { timeout: 30_000 }); await ctx.run.waitReached(parent, { timeout: 30_000 });
        await tui.key("C-c");
        await waitCall(ctx, parent, "client_abort");
        await assertOsc(tui, "working", "parent-cancelled-child-still-working");
        expect(threadState(ctx, actors.children.get("worker")!).agent_status).toBe("active");
        expect(ctx.proxy.snapshot(ctx.runId).calls.find((call: any) => call.identity.owner === "worker")?.terminal).toBeNull();
        actors.arm("continuation"); ctx.run.release(child);
        const done = await completed(ctx, tui, "B03_INDEPENDENT_RESULT_PROCESSED");
        expect(done).not.toContain("B03_CANCELLED_PARENT_FORBIDDEN_TAIL");
        expect(JSON.stringify(rootPayloads(ctx))).not.toContain("B03_CANCELLED_PARENT_FORBIDDEN_TAIL");
        await assertOsc(tui, "done", "continuation-finished");
        await assertFile(ctx, "b03.txt", "B03_SURVIVED_PARENT_CANCEL\n");
        assertCanonicalResultOnce(ctx, "B03_CHILD_COMPLETED_AFTER_PARENT_CANCEL");
        expect(countRootUserInputs(ctx)).toBe(1);
        expect(new Set(actors.seen.filter((call) => call.owner === ctx.sessionIds.root).map((call) => call.execution)).size).toBe(2);
        ctx.observe("cancel-policy-boundary", { parent: "client_abort", independentChild: "completed", userInputs: 1, rootExecutions: 2 });
      },
    });
  });

  it("B04 Tasks panel cancellation aborts the selected child before its Write and reports cancellation", async () => {
    let actors: AgentIdentities;
    const child = identity("worker", "reason:1"), parent = rootCall("reason:2");
    await runReplayScenario({ id: "B04", title: "真实 Tasks 面板取消 child，工具副作用不发生且取消可见", configVersion: CONFIG_VERSION,
      configure: configureBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.call === "reason:1") return tools(delegate("B04-spawn", "B04_CANCEL_SELECTED_CHILD", true));
        if (call.owner === "worker" && call.call === "reason:1") return tools(write("B04-forbidden-write", "b04-forbidden.txt", "FORBIDDEN\n"));
        if (call.owner === "root" && call.call === "reason:2") return tools(read("B04-probe", "b04-forbidden.txt"));
        if (call.owner === "root" && call.call === "reason:3") { expectResult(body, "B04-probe", "File not found", true); expect(JSON.stringify(body.messages).toLowerCase()).toContain("cancel"); return text("B04_CANCEL_CONFIRMED_NO_SIDE_EFFECT"); }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, [{ tool: "B04-spawn", alias: "worker", background: true }], [child, parent]); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const tui = await ctx.startTui("root", { rows: 55 });
        actors.arm("turn-1"); await tui.prompt("Execute B04.");
        await ctx.run.waitReached(child, { timeout: 30_000 }); await ctx.run.waitReached(parent, { timeout: 30_000 });
        await tui.command("tasks");
        await tui.waitFor(healthy(ctx, (screen) => screen.includes("B04_CANCEL_SELECTED_CHILD") && /cancel/iu.test(screen)), "Tasks panel offers selected child cancellation");
        await tui.save("tasks-selected-child");
        await tui.key("Enter");
        await waitCall(ctx, child, "client_abort");
        await tui.waitFor(healthy(ctx, (screen) => screen.includes("interrupted") && screen.includes("replay-worker")), "cancelled child is visibly interrupted after Tasks closes");
        ctx.run.release(parent);
        const screen = await completed(ctx, tui, "B04_CANCEL_CONFIRMED_NO_SIDE_EFFECT");
        expect(screen).toMatch(/[✗✕×]\s+Read/u);
        await assertMissingFile(ctx, "b04-forbidden.txt");
        expectResult(requestBody(ctx, rootCall("reason:3")), "B04-probe", "File not found", true);
        expect(threadState(ctx, actors.children.get("worker")!).agent_status).toBe("cancelled");
        expect(ctx.requests.filter((request) => request.context.owner === "worker")).toHaveLength(1);
        await assertOsc(tui, "done", "cancelled-child-parent-settled");
        ctx.observe("tasks-cancel-real-effects", { childRequest: "client_abort", fileAbsent: true, childStatus: "cancelled" });
      },
    });
  });

  it("B05 a nonretryable child model failure is delivered as failure and the parent can continue", async () => {
    let actors: AgentIdentities;
    const child = identity("worker", "reason:1"), parent = rootCall("reason:2");
    await runReplayScenario({ id: "B05", title: "异步 child 401 失败真实传回，父继续且无假成功", configVersion: CONFIG_VERSION,
      configure: configureBackground,
      upstream: async () => backgroundProvider((call, body) => {
        if (call.owner === "root" && call.call === "reason:1") return tools(delegate("B05-spawn", "B05_EXPECT_MODEL_FAILURE", true));
        if (call.owner === "worker" && call.call === "reason:1") return { status: 401, error: "B05_CHILD_AUTHENTICATION_REJECTED" };
        if (call.owner === "root" && call.call === "reason:2") return tools(write("B05-parent-write", "b05-parent.txt", "B05_PARENT_STILL_USABLE\n"));
        if (call.owner === "root" && call.call === "reason:3") { expectResult(body, "B05-parent-write", "b05-parent.txt"); expect(JSON.stringify(body.messages)).toContain("B05_CHILD_AUTHENTICATION_REJECTED"); return text("B05_FAILURE_HANDLED_BY_PARENT"); }
        return unexpected(call);
      }),
      runOptions(ctx) { const built = options(ctx, [{ tool: "B05-spawn", alias: "worker", background: true }], [child, parent]); actors = built.actors; return built.proxy; },
      async run(ctx) {
        const tui = await ctx.startTui("root", { rows: 55 });
        actors.arm("turn-1"); await tui.prompt("Execute B05.");
        await ctx.run.waitReached(child, { timeout: 30_000 }); await ctx.run.waitReached(parent, { timeout: 30_000 });
        ctx.run.release(child);
        await tui.waitFor(healthy(ctx, (screen) => /✗\s+agent\s+B05_EXPECT_MODEL_FAILURE/u.test(screen)), "child failure is visible in the real task area");
        await tui.save("child-failed-parent-pending");
        ctx.run.release(parent);
        await completed(ctx, tui, "B05_FAILURE_HANDLED_BY_PARENT");
        await assertFile(ctx, "b05-parent.txt", "B05_PARENT_STILL_USABLE\n");
        expect(threadState(ctx, actors.children.get("worker")!).agent_status).toBe("error");
        expect(JSON.stringify(requestBody(ctx, rootCall("reason:3")).messages)).toContain("B05_CHILD_AUTHENTICATION_REJECTED");
        expect(JSON.stringify(requestBody(ctx, rootCall("reason:3")).messages)).toContain("state: failed");
        assertCanonicalResultOnce(ctx, "B05_CHILD_AUTHENTICATION_REJECTED");
        const failure = rootPayloads(ctx).find((payload) => payload.type === "system_reminder" && JSON.stringify(payload).includes("B05_CHILD_AUTHENTICATION_REJECTED"));
        expect(failure).toMatchObject({ version: 1, type: "system_reminder", reminder: { category: "task", source: "subagent", delivery: "configurable",
          metadata: { success: false, timed_out: false, child_thread_id: actors.children.get("worker"), subagent_failure: { child_thread_id: actors.children.get("worker") } } } });
        expect(ctx.requests.filter((request) => request.context.owner === "worker")).toHaveLength(1);
        await assertOsc(tui, "done", "failure-handled-parent-done");
        ctx.observe("failed-child-outcome", { upstreamStatus: 401, childStatus: "error", retryCount: 0, parentUsable: true });
      },
    });
  });
});
