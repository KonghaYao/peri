import { describe, expect, it } from "vitest";
import { runReplayScenario, type ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import {
  AgentIdentities, agentProvider, assertChildTrace, assertFile, assertMissingFile, childIdFromResult,
  completed, configureAgent, delegate, expectResult, healthy, identity, persistedTrace,
  read, resultText, text, tool, tools, write,
  type CallIdentity, type ChildAnchor, type Reply,
} from "./agents-fixture.js";

function unexpected(call: CallIdentity): never { throw new Error(`unexpected business model call: ${JSON.stringify(call)}`); }
function options(context: ReplayScenarioContext, anchors: ChildAnchor[], gates: CallIdentity[] = []) {
  const actors = new AgentIdentities(context, anchors);
  return { actors, proxy: { resolveContext: actors.resolve, onRequest: actors.onRequest,
    gates: gates.map((call) => ({ identity: call, afterBytes: 0 })) } };
}
async function waitCompletedCall(context: ReplayScenarioContext, expected: CallIdentity) {
  await context.waitForCalls((state) => state.calls.some((call: any) =>
    call.identity.owner === expected.owner && call.identity.execution === expected.execution && call.identity.call === expected.call && call.terminal === "complete"));
}
function rootResult(context: ReplayScenarioContext, report: string) {
  const trace = persistedTrace(context);
  expect(trace.messages.some((entry) => entry.thread === trace.root && entry.message.role === "assistant"
    && JSON.stringify(entry.message.content).includes(report))).toBe(true);
}

describe("A01–A06 real Agent delegation through recorded HTTP and offline TUI replay", () => {
  it("A01 synchronous child creates a file and its completed toolcard remains visible", { timeout: 120_000 }, async () => {
    let actors: AgentIdentities;
    await runReplayScenario({
      id: "A01", title: "同步 child 创建、真实 Write 与完成工具卡", configure: configureAgent,
      upstream: async () => agentProvider((call, body): Reply => {
        if (call.owner === "root" && call.call === "reason:1") return tools(delegate("A01-spawn", "A01_CHILD_TASK"));
        if (call.owner === "worker" && call.call === "reason:1") return tools(write("A01-write", "a01-child.txt", "A01_CHILD_SIDE_EFFECT\n"));
        if (call.owner === "worker" && call.call === "reason:2") {
          expectResult(body, "A01-write", "a01-child.txt"); return text("A01_CHILD_REPORT_READY");
        }
        if (call.owner === "root" && call.call === "reason:2") {
          expectResult(body, "A01-spawn", "A01_CHILD_REPORT_READY"); return text("A01_PARENT_ACCEPTED_CHILD_RESULT");
        }
        return unexpected(call);
      }),
      runOptions(context) { const built = options(context, [{ tool: "A01-spawn", alias: "worker" }]); actors = built.actors; return built.proxy; },
      async run(context) {
        const tui = await context.startTui("root", { rows: 50 });
        actors.arm("turn-1"); await tui.prompt("Execute A01.");
        const screen = await completed(context, tui, "A01_PARENT_ACCEPTED_CHILD_RESULT");
        expect(screen).toMatch(/✓\s+Agent/u);
        expect(screen).toMatch(/✓\s+Write\s+.*a01-child\.txt/u);
        await assertFile(context, "a01-child.txt", "A01_CHILD_SIDE_EFFECT\n");
        const trace = assertChildTrace(context, actors, "worker", "A01-spawn", "a01-child.txt", "A01_CHILD_REPORT_READY");
        rootResult(context, "A01_PARENT_ACCEPTED_CHILD_RESULT");
        await tui.save("sync-child-completed");
        context.observe("independent-oracle", { ...trace, parentToolcardCompleted: true, file: "A01_CHILD_SIDE_EFFECT" });
      },
    });
  });

  it("A02 identical-name and identical-prompt children complete in reverse order without crossing results", { timeout: 120_000 }, async () => {
    let actors: AgentIdentities;
    const heldLeft = identity("left", "reason:2");
    await runReplayScenario({
      id: "A02", title: "同名同 prompt 双 child 并行，逆序完成且不串结果", configure: configureAgent,
      upstream: async () => agentProvider((call, body): Reply => {
        if (call.owner === "root" && call.call === "reason:1") return tools(
          delegate("A02-left", "A02_IDENTICAL_CHILD_TASK"), delegate("A02-right", "A02_IDENTICAL_CHILD_TASK"));
        if (["left", "right"].includes(call.owner) && call.call === "reason:1") {
          return tools(write(`A02-write-${call.owner}`, `a02-${call.owner}.txt`, `A02_${call.owner.toUpperCase()}_SIDE_EFFECT\n`));
        }
        if (["left", "right"].includes(call.owner) && call.call === "reason:2") {
          expectResult(body, `A02-write-${call.owner}`, `a02-${call.owner}.txt`);
          return text(`A02_${call.owner.toUpperCase()}_REPORT`);
        }
        if (call.owner === "root" && call.call === "reason:2") {
          expectResult(body, "A02-left", "A02_LEFT_REPORT"); expectResult(body, "A02-right", "A02_RIGHT_REPORT");
          expect(resultText(body, "A02-left")).not.toContain("A02_RIGHT_REPORT");
          expect(resultText(body, "A02-right")).not.toContain("A02_LEFT_REPORT");
          return text("A02_BOTH_RESULTS_CORRECTLY_OWNED");
        }
        return unexpected(call);
      }),
      runOptions(context) {
        const built = options(context, [{ tool: "A02-left", alias: "left" }, { tool: "A02-right", alias: "right" }], [heldLeft]);
        actors = built.actors; return built.proxy;
      },
      async run(context) {
        const tui = await context.startTui("root", { rows: 60, cols: 140 });
        actors.arm("turn-1"); await tui.prompt("Execute A02.");
        await context.run.waitReached(heldLeft, { timeout: 30_000 });
        await waitCompletedCall(context, identity("right", "reason:2"));
        const blocked = await tui.waitFor(healthy(context, (screen) => /✓\s+Agent/u.test(screen)
          && screen.includes("a02-right.txt") && screen.includes("a02-left.txt")), "right child completed while left response is gated");
        expect(blocked).not.toContain("A02_BOTH_RESULTS_CORRECTLY_OWNED");
        expect(context.proxy.snapshot(context.runId).calls.find((call: any) => call.identity.owner === "left" && call.identity.call === "reason:2")?.terminal).toBeNull();
        await tui.save("right-finished-left-held");
        context.run.release(heldLeft);
        const done = await completed(context, tui, "A02_BOTH_RESULTS_CORRECTLY_OWNED");
        expect((done.match(/✓\s+Agent/gu) ?? []).length).toBe(2);
        for (const alias of ["left", "right"]) {
          await assertFile(context, `a02-${alias}.txt`, `A02_${alias.toUpperCase()}_SIDE_EFFECT\n`);
          const trace = assertChildTrace(context, actors, alias, `A02-${alias}`, `a02-${alias}.txt`, `A02_${alias.toUpperCase()}_REPORT`);
          expect(trace.childTools).toEqual([{ name: "Write", file: `a02-${alias}.txt` }]);
        }
        expect(persistedTrace(context).children).toHaveLength(2);
        context.observe("independent-oracle", { childCount: 2, samePrompt: true, firstCompleted: "right", rootResultMapping: "left→LEFT,right→RIGHT" });
      },
    });
  });

  it("A03 background child reports through the parent mailbox while the parent continues", { timeout: 120_000 }, async () => {
    let actors: AgentIdentities;
    const heldChild = identity("worker", "reason:1"), heldParent = identity("root", "reason:2", "turn-1");
    await runReplayScenario({
      id: "A03", title: "异步 child 完成并通过 mailbox 回送主会话", configure: configureAgent,
      upstream: async () => agentProvider((call, body): Reply => {
        if (call.owner === "root" && call.call === "reason:1") return tools(delegate("A03-spawn", "A03_BACKGROUND_CHILD", true));
        if (call.owner === "worker" && call.call === "reason:1") return tools(write("A03-write", "a03-child.txt", "A03_BACKGROUND_FILE\n"));
        if (call.owner === "worker" && call.call === "reason:2") { expectResult(body, "A03-write", "a03-child.txt"); return text("A03_CHILD_FINISHED_REPORT"); }
        if (call.owner === "root" && call.call === "reason:2") {
          expectResult(body, "A03-spawn", "Background task"); return tools(read("A03-read-parent", "a03-child.txt"));
        }
        if (call.owner === "root" && call.call === "reason:3") {
          expectResult(body, "A03-read-parent", "A03_BACKGROUND_FILE");
          expect(JSON.stringify(body.messages)).toContain("A03_CHILD_FINISHED_REPORT");
          return text("A03_PARENT_RECEIVED_BACKGROUND_REPORT");
        }
        return unexpected(call);
      }),
      runOptions(context) {
        const built = options(context, [{ tool: "A03-spawn", alias: "worker", background: true }], [heldChild, heldParent]);
        actors = built.actors; return built.proxy;
      },
      async run(context) {
        const tui = await context.startTui("root", { rows: 55 });
        actors.arm("turn-1"); await tui.prompt("Execute A03.");
        await context.run.waitReached(heldChild, { timeout: 30_000 });
        await context.run.waitReached(heldParent, { timeout: 30_000 });
        await tui.save("parent-continues-child-pending");
        expect(context.proxy.snapshot(context.runId).calls.some((call: any) => call.identity.owner === "worker" && call.terminal === "complete")).toBe(false);
        context.run.release(heldChild);
        await waitCompletedCall(context, identity("worker", "reason:2"));
        await tui.waitFor(healthy(context, (screen) => /✔\s+agent\s+A03_BACKGROUND_CHILD/u.test(screen)), "durable background completion reached the real TUI task area");
        context.run.release(heldParent);
        const done = await completed(context, tui, "A03_PARENT_RECEIVED_BACKGROUND_REPORT");
        expect(done).toMatch(/✓\s+Read\s+.*a03-child\.txt/u);
        await assertFile(context, "a03-child.txt", "A03_BACKGROUND_FILE\n");
        assertChildTrace(context, actors, "worker", "A03-spawn", "a03-child.txt", "A03_CHILD_FINISHED_REPORT");
        const trace = persistedTrace(context);
        expect(JSON.stringify(trace.messages.filter((entry) => entry.thread === trace.root))).toContain("A03_CHILD_FINISHED_REPORT");
        context.observe("independent-oracle", { parentContinuedBeforeChild: true, backgroundReportPersisted: true, file: "A03_BACKGROUND_FILE" });
      },
    });
  });

  it("A04 resume uses the same persisted child and a new execution on a second user turn", { timeout: 120_000 }, async () => {
    let actors: AgentIdentities;
    await runReplayScenario({
      id: "A04", title: "第二轮 resume 同一 child，portable 重绑定真实 UUID", configure: configureAgent,
      upstream: async () => agentProvider((call, body): Reply => {
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:1") return tools(delegate("A04-spawn", "A04_FIRST_CHILD_TASK"));
        if (call.owner === "worker" && call.execution === "created" && call.call === "reason:1") return tools(write("A04-write-first", "a04-first.txt", "A04_FIRST_EXECUTION\n"));
        if (call.owner === "worker" && call.execution === "created" && call.call === "reason:2") { expectResult(body, "A04-write-first", "a04-first.txt"); return text("A04_CHILD_FIRST_REPORT"); }
        if (call.owner === "root" && call.execution === "turn-1" && call.call === "reason:2") { expectResult(body, "A04-spawn", "A04_CHILD_FIRST_REPORT"); return text("A04_FIRST_TURN_COMPLETE"); }
        if (call.owner === "root" && call.execution === "turn-2" && call.call === "reason:1") return tools(tool("A04-resume", "Agent", { resume_thread_id: childIdFromResult(body, "A04-spawn"), prompt: "A04_RESUMED_CHILD_TASK", run_in_background: false }));
        if (call.owner === "worker" && call.execution === "resumed" && call.call === "reason:1") {
          expect(JSON.stringify(body.messages)).toContain("A04_CHILD_FIRST_REPORT");
          return tools(write("A04-write-second", "a04-second.txt", "A04_SAME_CHILD_RESUMED\n"));
        }
        if (call.owner === "worker" && call.execution === "resumed" && call.call === "reason:2") { expectResult(body, "A04-write-second", "a04-second.txt"); return text("A04_CHILD_RESUME_REPORT"); }
        if (call.owner === "root" && call.execution === "turn-2" && call.call === "reason:2") {
          expectResult(body, "A04-resume", "action: resume"); expectResult(body, "A04-resume", "A04_CHILD_RESUME_REPORT");
          expect(childIdFromResult(body, "A04-resume")).toBe(childIdFromResult(body, "A04-spawn"));
          return text("A04_RESUMED_EXISTING_CHILD_CONFIRMED");
        }
        return unexpected(call);
      }),
      runOptions(context) {
        const built = options(context, [{ tool: "A04-spawn", alias: "worker" }, { tool: "A04-resume", alias: "worker", action: "turn-2", execution: "resumed" }]);
        actors = built.actors; return built.proxy;
      },
      async run(context) {
        const tui = await context.startTui("root", { rows: 60 });
        actors.arm("turn-1"); await tui.prompt("Execute A04 first phase.");
        await completed(context, tui, "A04_FIRST_TURN_COMPLETE");
        const originalChild = actors.children.get("worker");
        actors.arm("turn-2"); await tui.prompt("Execute A04 second phase.");
        const done = await completed(context, tui, "A04_RESUMED_EXISTING_CHILD_CONFIRMED");
        expect(done).toContain("a04-second.txt");
        expect(actors.children.get("worker")).toBe(originalChild);
        expect(persistedTrace(context).children).toHaveLength(1);
        await assertFile(context, "a04-first.txt", "A04_FIRST_EXECUTION\n");
        await assertFile(context, "a04-second.txt", "A04_SAME_CHILD_RESUMED\n");
        assertChildTrace(context, actors, "worker", "A04-resume", "a04-second.txt", "A04_CHILD_RESUME_REPORT");
        const executions = new Set(actors.seen.filter((call) => call.owner === originalChild).map((call) => call.execution));
        expect(executions.size).toBe(2);
        context.observe("independent-oracle", { children: 1, childExecutions: 2, firstFile: "A04_FIRST_EXECUTION", resumedFile: "A04_SAME_CHILD_RESUMED" });
      },
    });
  });

  it("A05 a child observes a real failed Read and recovers with a successful Write", { timeout: 120_000 }, async () => {
    let actors: AgentIdentities;
    await runReplayScenario({
      id: "A05", title: "child 工具失败后真实恢复，错误与成功卡片并存", configure: configureAgent,
      upstream: async () => agentProvider((call, body): Reply => {
        if (call.owner === "root" && call.call === "reason:1") return tools(delegate("A05-spawn", "A05_RECOVER_CHILD_TASK"));
        if (call.owner === "worker" && call.call === "reason:1") return tools(read("A05-failed-read", "a05-does-not-exist.txt"));
        if (call.owner === "worker" && call.call === "reason:2") {
          expectResult(body, "A05-failed-read", "a05-does-not-exist.txt", true);
          expectResult(body, "A05-failed-read", "File not found", true);
          return tools(write("A05-recover-write", "a05-recovered.txt", "A05_RECOVERY_SIDE_EFFECT\n"));
        }
        if (call.owner === "worker" && call.call === "reason:3") { expectResult(body, "A05-recover-write", "a05-recovered.txt"); return text("A05_CHILD_RECOVERED_REPORT"); }
        if (call.owner === "root" && call.call === "reason:2") { expectResult(body, "A05-spawn", "A05_CHILD_RECOVERED_REPORT"); return text("A05_RECOVERED_CHILD_RESULT_ACCEPTED"); }
        return unexpected(call);
      }),
      runOptions(context) { const built = options(context, [{ tool: "A05-spawn", alias: "worker" }]); actors = built.actors; return built.proxy; },
      async run(context) {
        await assertMissingFile(context, "a05-does-not-exist.txt");
        const tui = await context.startTui("root", { rows: 55 });
        actors.arm("turn-1"); await tui.prompt("Execute A05.");
        const done = await completed(context, tui, "A05_RECOVERED_CHILD_RESULT_ACCEPTED");
        expect(done).toMatch(/[✗✕×]\s+Read\s+.*a05-does-not-exist\.txt/u);
        expect(done).toMatch(/✓\s+Write\s+.*a05-recovered\.txt/u);
        await assertFile(context, "a05-recovered.txt", "A05_RECOVERY_SIDE_EFFECT\n");
        assertChildTrace(context, actors, "worker", "A05-spawn", "a05-recovered.txt", "A05_CHILD_RECOVERED_REPORT");
        const child = actors.children.get("worker");
        const failed = persistedTrace(context).messages.find((entry) => entry.thread === child && entry.message.role === "tool" && entry.message.tool_call_id === "A05-failed-read");
        expect(failed).toBeDefined();
        expect(failed!.message.is_error).toBe(true);
        expect(JSON.stringify(failed!.message.content)).toContain("File not found");
        context.observe("independent-oracle", { readFailed: true, childRecovered: true, file: "A05_RECOVERY_SIDE_EFFECT" });
      },
    });
  });

  it("A06 controlled parent-child file contention obeys the declared happens-before order", { timeout: 120_000 }, async () => {
    let actors: AgentIdentities;
    const heldChild = identity("worker", "reason:1"), heldParent = identity("root", "reason:3", "turn-1");
    await runReplayScenario({
      id: "A06", title: "父子共享文件，受控因果顺序决定最终内容", configure: configureAgent,
      upstream: async () => agentProvider((call, body): Reply => {
        if (call.owner === "root" && call.call === "reason:1") return tools(write("A06-initial-write", "a06-shared.txt", "A06_PARENT_INITIAL\n"));
        if (call.owner === "root" && call.call === "reason:2") {
          expectResult(body, "A06-initial-write", "a06-shared.txt");
          return tools(delegate("A06-spawn", "A06_SHARED_FILE_CHILD", true), write("A06-parent-write", "a06-shared.txt", "A06_PARENT_BEFORE_CHILD\n"));
        }
        if (call.owner === "worker" && call.call === "reason:1") return tools(write("A06-child-write", "a06-shared.txt", "A06_CHILD_AFTER_PARENT\n"));
        if (call.owner === "worker" && call.call === "reason:2") { expectResult(body, "A06-child-write", "a06-shared.txt"); return text("A06_CHILD_FINAL_WRITE_REPORT"); }
        if (call.owner === "root" && call.call === "reason:3") {
          expectResult(body, "A06-parent-write", "a06-shared.txt"); expectResult(body, "A06-spawn", "Background task");
          return tools(read("A06-parent-read", "a06-shared.txt"));
        }
        if (call.owner === "root" && call.call === "reason:4") {
          expectResult(body, "A06-parent-read", "A06_CHILD_AFTER_PARENT");
          expect(JSON.stringify(body.messages)).toContain("A06_CHILD_FINAL_WRITE_REPORT");
          return text("A06_CAUSAL_FINAL_FILE_CONFIRMED");
        }
        return unexpected(call);
      }),
      runOptions(context) {
        const built = options(context, [{ tool: "A06-spawn", alias: "worker", parentCall: "reason:2", background: true }], [heldChild, heldParent]);
        actors = built.actors; return built.proxy;
      },
      async run(context) {
        const tui = await context.startTui("root", { rows: 60 });
        actors.arm("turn-1"); await tui.prompt("Execute A06.");
        await context.run.waitReached(heldChild, { timeout: 30_000 });
        await context.run.waitReached(heldParent, { timeout: 30_000 });
        await assertFile(context, "a06-shared.txt", "A06_PARENT_BEFORE_CHILD\n");
        await tui.save("parent-write-before-child-release");
        context.run.release(heldChild);
        await waitCompletedCall(context, identity("worker", "reason:2"));
        await tui.waitFor(healthy(context, (screen) => /✔\s+agent\s+A06_SHARED_FILE_CHILD/u.test(screen)), "durable child completion before parent read");
        await assertFile(context, "a06-shared.txt", "A06_CHILD_AFTER_PARENT\n");
        context.run.release(heldParent);
        const done = await completed(context, tui, "A06_CAUSAL_FINAL_FILE_CONFIRMED");
        expect(done).toMatch(/✓\s+Read\s+.*a06-shared\.txt/u);
        assertChildTrace(context, actors, "worker", "A06-spawn", "a06-shared.txt", "A06_CHILD_FINAL_WRITE_REPORT");
        rootResult(context, "A06_CAUSAL_FINAL_FILE_CONFIRMED");
        context.observe("independent-oracle", { beforeChildRelease: "A06_PARENT_BEFORE_CHILD", finalFile: "A06_CHILD_AFTER_PARENT", parentReadConfirmedFinal: true });
      },
    });
  });
});
