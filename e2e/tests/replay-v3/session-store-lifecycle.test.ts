import { cp, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { createReplayWorld, prepareReplaySessions, ReplayTui } from "../../helpers/llm-replay-tui.js";
import { readReplayStore } from "../../helpers/replay-isolation.js";
import { bodyOf, identity, lastUserText, preserveAndRemoveAuxiliaryWorld, responsePrefix, results, sessionModel, sessionPeer, stored, turn } from "./session-store-fixture.js";

describe("replay v3 · durable history across deployment generations", () => {
  it("D06 crashing a working session creates a new recovery execution without repeating committed tools", async () => {
    const interrupted = { ...identity("root", "interrupted-process"), call: "reason:2" };
    await runReplayScenario({ id: "D06", title: "真实副作用后流中 SIGKILL，working 以新执行续跑且旧工具仅执行一次",
      upstream: async () => sessionModel((body, index) => {
        if (lastUserText(body).includes("D06_NEW_INPUT")) return { text: "D06_NEW_EXECUTION_FINISHED" };
        if (index === 1) return { tool: { id: "D06-committed-tool", name: "Bash", input: {
          command: "printf '%s\\n' D06_EFFECT_ONCE >> crash-effects.txt; printf '%s\\n' D06_COMMITTED_TOOL_RESULT",
        } } };
        expect(results(body).filter((result) => result.tool_use_id === "D06-committed-tool")).toHaveLength(1);
        if (index === 2) return { text: "D06_VISIBLE_PREFIX", tail: " D06_FORBIDDEN_OLD_TAIL" };
        expect(index).toBe(3);
        expect(JSON.stringify(body.messages)).toContain("旧工具调用不会自动重放");
        expect(JSON.stringify(body.messages)).toContain("已确认当前没有仍存活的 Shell 任务");
        expect(JSON.stringify(body.messages)).not.toContain("外围任务归属与存活状态：未知");
        return { text: "D06_RECOVERED_NEW_EXECUTION" };
      }),
      runOptions: () => ({ gates: [{ identity: interrupted, afterBytes: Buffer.byteLength(responsePrefix("D06_VISIBLE_PREFIX")) }] }),
      async run(ctx) {
        let tui = await ctx.startTui("root", { label: "interrupted-process" });
        ctx.identity.arm(ctx.sessionIds.root!, "interrupted-process"); await tui.prompt("D06_OLD_USER_INPUT");
        await ctx.run.waitReached(interrupted);
        const partial = await tui.waitFor(ctx.checked((screen: string) => screen.includes("D06_VISIBLE_PREFIX")), "real partial stream rendered");
        expect(partial).not.toContain("D06_FORBIDDEN_OLD_TAIL"); await tui.save("before-termination");
        expect(await readFile(path.join(ctx.world.work, "crash-effects.txt"), "utf8")).toBe("D06_EFFECT_ONCE\n");
        const outcome = await tui.crash(); expect(outcome.exitCode).toBe(-9); expect(outcome.shutdown).toBe("crash");
        await ctx.waitForCalls((snapshot) => snapshot.active === 0 && snapshot.calls.length === 2
          && snapshot.calls.filter((call: any) => call.terminal === "client_abort").length === 1);
        ctx.identity.finish(ctx.sessionIds.root!);
        const prior = stored(ctx.world, ctx.sessionIds.root!);
        expect(prior.filter((value) => value.includes("D06_OLD_USER_INPUT"))).toHaveLength(1);
        expect(prior.join()).not.toContain("D06_FORBIDDEN_OLD_TAIL");
        expect(readReplayStore(ctx.world).threads.find((thread) => thread.id === ctx.sessionIds.root)?.work_state).toBe("working");
        ctx.identity.arm(ctx.sessionIds.root!, "recovered-execution");
        tui = await ctx.startTui("root", { label: "after-termination" });
        const restored = await tui.waitFor(ctx.checked((screen: string) => screen.includes("D06_RECOVERED_NEW_EXECUTION")
          && screen.includes("Brewed for")), "working intent completes in a new runtime");
        expect(restored).not.toContain("D06_FORBIDDEN_OLD_TAIL");
        await ctx.waitForCalls((snapshot) => snapshot.active === 0 && snapshot.calls.length === 3);
        ctx.identity.finish(ctx.sessionIds.root!); await tui.save("recovered-execution");
        expect(await readFile(path.join(ctx.world.work, "crash-effects.txt"), "utf8")).toBe("D06_EFFECT_ONCE\n");
        expect(ctx.requests[1].context.execution).toBe("interrupted-process");
        expect(ctx.requests[2].context.execution).toBe("recovered-execution");
        expect(readReplayStore(ctx.world).threads.find((thread) => thread.id === ctx.sessionIds.root)?.work_state).toBe("idle");
        const complete = await turn(ctx, tui, "root", "new-execution", "D06_NEW_INPUT", "D06_NEW_EXECUTION_FINISHED", 4);
        expect(complete).not.toContain("D06_FORBIDDEN_OLD_TAIL");
        const history = JSON.stringify(bodyOf(ctx.requests[3]).messages);
        expect(history).toContain("D06_OLD_USER_INPUT"); expect(history).not.toContain("D06_FORBIDDEN_OLD_TAIL");
        expect(stored(ctx.world, ctx.sessionIds.root!).filter((value) => value.includes("D06_OLD_USER_INPUT"))).toHaveLength(1);
        expect(await readFile(path.join(ctx.world.work, "crash-effects.txt"), "utf8")).toBe("D06_EFFECT_ONCE\n");
        ctx.observe("history_after_interrupted_process", { outcome, prior, final: readReplayStore(ctx.world) });
      } });
  });

  it("D07 copying the working files into a new store never copies conversation history", async () => {
    await runReplayScenario({ id: "D07", title: "同样工作文件进入独立临时 store，真实工具可读但旧会话历史不可见",
      configure: async (world) => { await writeFile(path.join(world.work, "shared.txt"), "D07_SHARED_WORKSPACE_FILE\n"); },
      upstream: async () => sessionModel((body) => {
        if (lastUserText(body).includes("D07_ORIGINAL_INPUT")) return { text: "D07_ORIGINAL_PRIVATE_HISTORY" };
        if (!results(body).length) return { tool: { id: "D07-read", name: "Read", input: { file_path: "shared.txt" } } };
        expect(results(body)).toHaveLength(1); expect(results(body)[0].is_error).not.toBe(true);
        expect(JSON.stringify(results(body)[0].content)).toContain("D07_SHARED_WORKSPACE_FILE");
        return { text: "D07_NEW_STORE_READ_COMPLETED" };
      }), async run(ctx) {
        const original = await ctx.startTui("root");
        await turn(ctx, original, "root", "original", "D07_ORIGINAL_INPUT", "D07_ORIGINAL_PRIVATE_HISTORY", 1);
        await original.stop();
        const clean = await createReplayWorld(); let cleanTui: ReplayTui | undefined;
        clean.evidence = path.join(ctx.world.evidence, "clean-store"); await mkdir(clean.evidence, { recursive: true });
        try {
          await cp(ctx.world.work, clean.work, { recursive: true });
          expect(await readFile(path.join(clean.work, "shared.txt"), "utf8")).toBe("D07_SHARED_WORKSPACE_FILE\n");
          const [id] = await prepareReplaySessions(clean); expect(id).not.toBe(ctx.sessionIds.root);
          ctx.identity.register(id!, "clean", "session/new in a distinct temporary store with copied working files only");
          for (const field of ["root", "home", "work"] as const) ctx.bind({ kind: "path", alias: `clean-${field}`, live: clean[field], anchor: `independent clean store ${field}` });
          const before = readReplayStore(clean); expect(before.messages).toEqual([]);
          cleanTui = await ReplayTui.start(clean, { label: "clean-session", sessionId: id!, proxy: ctx.proxy.url, run: ctx.runId });
          const initial = await cleanTui.screen(); expect(initial).not.toContain("D07_ORIGINAL_PRIVATE_HISTORY");
          const screen = await turn(ctx, cleanTui, "clean", "read", "D07_READ_COPIED_WORKSPACE", "D07_NEW_STORE_READ_COMPLETED", 3);
          expect(screen).toMatch(/✓\s+Read/); expect(screen).not.toContain("D07_ORIGINAL_PRIVATE_HISTORY");
          for (const request of ctx.requests.slice(1)) expect(JSON.stringify(bodyOf(request).messages)).not.toContain("D07_ORIGINAL_PRIVATE_HISTORY");
          const after = readReplayStore(clean); expect(after.integrity).toBe("ok");
          expect(after.messages.map((row) => row.content).join()).not.toContain("D07_ORIGINAL_PRIVATE_HISTORY");
          expect(stored(ctx.world, ctx.sessionIds.root!).join()).not.toContain("D07_NEW_STORE_READ_COMPLETED");
          ctx.observe("independent_store_history_and_files", { before, after, primary: readReplayStore(ctx.world) });
        } finally {
          try { await cleanTui?.stop(); }
          finally { await preserveAndRemoveAuxiliaryWorld(ctx, clean, "clean-store"); }
        }
      } });
  });

  it("D08 a terminal model failure survives process restart as history, not an automatic retry", async () => {
    await runReplayScenario({ id: "D08", title: "模型终态错误后重启，失败输入不重发，新输入继续原历史",
      upstream: async () => sessionModel((body) => lastUserText(body).includes("D08_CONTINUE")
        ? { text: "D08_RECOVERED_IN_NEW_PROCESS" }
        : { error: { status: 400, type: "invalid_request_error", message: "D08_EXPECTED_MODEL_REJECTION" } }),
      async run(ctx) {
        let tui = await ctx.startTui("root", { label: "failed-process" });
        ctx.identity.arm(ctx.sessionIds.root!, "failed"); await tui.prompt("D08_FAIL_THIS_INPUT");
        const failure = await tui.waitFor(ctx.checked((screen: string) => screen.includes("400") && screen.includes("D08_EXPECTED_MODEL_REJECTION")), "specific model failure visible");
        expect(failure).not.toContain("D08_RECOVERED_IN_NEW_PROCESS"); await tui.save("model-failure");
        await ctx.waitForCalls((snapshot) => snapshot.active === 0 && snapshot.calls.length === 1); ctx.identity.finish(ctx.sessionIds.root!);
        await tui.stop();
        const failedStore = readReplayStore(ctx.world); expect(failedStore.integrity).toBe("ok");
        expect(failedStore.threads.find((thread) => thread.id === ctx.sessionIds.root)?.work_state).toBe("idle");
        expect(stored(ctx.world, ctx.sessionIds.root!).filter((value) => value.includes("D08_FAIL_THIS_INPUT"))).toHaveLength(1);
        tui = await ctx.startTui("root", { label: "recovered-process" });
        await tui.waitFor(ctx.checked((screen: string) => screen.includes("D08_FAIL_THIS_INPUT")), "failed request remains history");
        expect(ctx.requests).toHaveLength(1);
        await turn(ctx, tui, "root", "continue", "D08_CONTINUE", "D08_RECOVERED_IN_NEW_PROCESS", 2);
        expect(JSON.stringify(bodyOf(ctx.requests[1]).messages)).toContain("D08_FAIL_THIS_INPUT");
        expect(stored(ctx.world, ctx.sessionIds.root!).filter((value) => value.includes("D08_FAIL_THIS_INPUT"))).toHaveLength(1);
        ctx.observe("terminal_failure_then_new_generation", { failedStore, recoveredStore: readReplayStore(ctx.world) });
      } });
  });

  it("D09 a persisted tool failure is preserved while a repaired workspace is read again after restart", async () => {
    await runReplayScenario({ id: "D09", title: "工具失败历史跨进程保存，工作文件修复后真实重读且不覆盖旧错误",
      upstream: async () => sessionModel((body) => {
        const repaired = lastUserText(body).includes("D09_READ_REPAIRED");
        const toolId = repaired ? "D09-after" : "D09-before";
        const found = results(body).filter((result) => result.tool_use_id === toolId);
        if (!found.length) return { tool: { id: toolId, name: "Read", input: { file_path: "repair-later.txt" } } };
        expect(found).toHaveLength(1);
        if (!repaired) {
          expect(found[0].is_error).toBe(true); expect(JSON.stringify(found[0].content)).toContain("Error: File not found at ");
          return { text: "D09_MISSING_FILE_RECORDED" };
        }
        expect(found[0].is_error).not.toBe(true); expect(JSON.stringify(found[0].content)).toContain("D09_EXTERNALLY_REPAIRED_CONTENT");
        return { text: "D09_REPAIRED_FILE_READ" };
      }), async run(ctx) {
        let tui = await ctx.startTui("root", { label: "missing-file" });
        await turn(ctx, tui, "root", "missing", "D09_READ_MISSING", "D09_MISSING_FILE_RECORDED", 2); await tui.stop();
        const failureRows = stored(ctx.world, ctx.sessionIds.root!);
        expect(failureRows.join()).toContain("File not found"); expect(failureRows.join()).toContain("D09-before");
        await writeFile(path.join(ctx.world.work, "repair-later.txt"), "D09_EXTERNALLY_REPAIRED_CONTENT\n");
        tui = await ctx.startTui("root", { label: "repaired-file" });
        await tui.waitFor((screen) => screen.includes("D09_MISSING_FILE_RECORDED"), "old failed tool round restored");
        await turn(ctx, tui, "root", "repaired", "D09_READ_REPAIRED", "D09_REPAIRED_FILE_READ", 4);
        const current = bodyOf(ctx.requests[3]);
        const before = results(current).filter((result) => result.tool_use_id === "D09-before");
        const after = results(current).filter((result) => result.tool_use_id === "D09-after");
        expect(before).toHaveLength(1); expect(before[0].is_error).toBe(true); expect(JSON.stringify(before[0].content)).toContain("File not found");
        expect(after).toHaveLength(1); expect(after[0].is_error).not.toBe(true); expect(JSON.stringify(after[0].content)).toContain("D09_EXTERNALLY_REPAIRED_CONTENT");
        expect(stored(ctx.world, ctx.sessionIds.root!).filter((value) => value.includes("D09_REPAIRED_FILE_READ"))).toHaveLength(1);
        ctx.observe("failed_and_successful_tool_results", { before, after, store: readReplayStore(ctx.world) });
      } });
  });

  it("D10 forked history is a stable snapshot across later parent and fork process generations", async () => {
    await runReplayScenario({ id: "D10", title: "真实 fork 固定历史快照，父与 fork 后续独立迭代不串扰",
      upstream: async () => sessionModel((body) => {
        const prompt = lastUserText(body);
        return { text: prompt.includes("D10_FORK_LATER_INPUT") ? "D10_FORK_LATER_REPLY"
          : prompt.includes("D10_PARENT_LATER_INPUT") ? "D10_PARENT_LATER_REPLY" : "D10_SHARED_FORK_POINT" };
      }), async run(ctx) {
        let root = await ctx.startTui("root", { label: "before-fork" });
        await turn(ctx, root, "root", "seed", "D10_SEED_INPUT", "D10_SHARED_FORK_POINT", 1); await root.stop();
        const peer = await sessionPeer(ctx.world, { proxy: ctx.proxy.url, run: ctx.runId });
        let forkId: string;
        try {
          const loaded = await peer.call("session/load", { sessionId: ctx.sessionIds.root, cwd: ctx.world.work, mcpServers: [] });
          expect(loaded.error, JSON.stringify(loaded)).toBeUndefined();
          const forked = await peer.call("session/fork", { sessionId: ctx.sessionIds.root, cwd: ctx.world.work });
          expect(forked.error, JSON.stringify(forked)).toBeUndefined(); forkId = forked.result.sessionId;
          expect(typeof forkId).toBe("string"); expect(forkId).not.toBe(ctx.sessionIds.root);
          expect((await peer.call("session/close", { sessionId: forkId })).error).toBeUndefined();
          expect((await peer.call("session/close", { sessionId: ctx.sessionIds.root })).error).toBeUndefined();
        } finally { expect(await peer.close()).toBe(0); }
        ctx.identity.register(forkId!, "fork", "real ACP session/fork response for root at its first completed round");
        expect(ctx.requests).toHaveLength(1);
        root = await ctx.startTui("root", { label: "parent-after-fork" });
        await turn(ctx, root, "root", "parent-later", "D10_PARENT_LATER_INPUT", "D10_PARENT_LATER_REPLY", 2); await root.stop();
        const fork = await ctx.startTui("fork", { label: "fork-after-parent-advanced" });
        const restored = await fork.waitFor((screen) => screen.includes("D10_SHARED_FORK_POINT"), "fork retains its frozen prefix");
        expect(restored).not.toContain("D10_PARENT_LATER_REPLY");
        await turn(ctx, fork, "fork", "fork-later", "D10_FORK_LATER_INPUT", "D10_FORK_LATER_REPLY", 3); await fork.stop();
        const history = JSON.stringify(bodyOf(ctx.requests[2]).messages);
        expect(history).toContain("D10_SHARED_FORK_POINT"); expect(history).not.toContain("D10_PARENT_LATER_INPUT"); expect(history).not.toContain("D10_PARENT_LATER_REPLY");
        expect(stored(ctx.world, ctx.sessionIds.root!).join()).not.toContain("D10_FORK_LATER_REPLY");
        expect(stored(ctx.world, forkId!).join()).not.toContain("D10_PARENT_LATER_REPLY");
        root = await ctx.startTui("root", { label: "parent-final-read" });
        const parent = await root.waitFor((screen) => screen.includes("D10_PARENT_LATER_REPLY"), "parent remains on its own later branch");
        expect(parent).not.toContain("D10_FORK_LATER_REPLY"); expect(ctx.requests).toHaveLength(3);
        expect(readReplayStore(ctx.world).threads).toHaveLength(2); ctx.observe("fork_branches_store", readReplayStore(ctx.world));
      } });
  });

  it("D11 an explicit user stop survives cold load until a new user intent resumes the session", async () => {
    await runReplayScenario({ id: "D11", title: "用户 Stop 跨进程保留 stopped，不自动续跑；新输入才解除停止并完成",
      upstream: async () => sessionModel((body) => lastUserText(body).includes("D11_EXPLICIT_NEW_INTENT")
        ? { text: "D11_NEW_INTENT_COMPLETED" } : { text: "D11_VISIBLE_BEFORE_STOP", tail: " D11_FORBIDDEN_CANCELLED_TAIL" }),
      runOptions: () => ({ gates: [{ identity: identity("root", "stopped-execution"),
        afterBytes: Buffer.byteLength(responsePrefix("D11_VISIBLE_BEFORE_STOP")) }] }),
      async run(ctx) {
        let tui = await ctx.startTui("root", { label: "before-user-stop" });
        ctx.identity.arm(ctx.sessionIds.root!, "stopped-execution"); await tui.prompt("D11_OLD_USER_INPUT");
        await ctx.run.waitReached(identity("root", "stopped-execution"));
        await tui.waitFor(ctx.checked((screen: string) => screen.includes("D11_VISIBLE_BEFORE_STOP")), "partial reply before user stop");
        await tui.key("C-c");
        await ctx.waitForCalls((snapshot) => snapshot.active === 0 && snapshot.calls.length === 1 && snapshot.calls[0].terminal === "client_abort");
        ctx.identity.finish(ctx.sessionIds.root!);
        const cancelled = await tui.waitFor(ctx.checked((screen: string) => screen.includes("Brewed for")), "user stop leaves the busy state");
        expect(cancelled).not.toContain("D11_FORBIDDEN_CANCELLED_TAIL"); await tui.save("user-stopped"); await tui.stop();
        const stopped = readReplayStore(ctx.world);
        expect(stopped.threads.find((thread) => thread.id === ctx.sessionIds.root)?.work_state).toBe("stopped");
        tui = await ctx.startTui("root", { label: "cold-stopped-session" });
        const history = await tui.waitFor(ctx.checked((screen: string) => screen.includes("D11_OLD_USER_INPUT")), "explicitly stopped history loads");
        expect(history).not.toContain("D11_FORBIDDEN_CANCELLED_TAIL"); expect(ctx.requests).toHaveLength(1);
        expect(readReplayStore(ctx.world).threads.find((thread) => thread.id === ctx.sessionIds.root)?.work_state).toBe("stopped");
        await turn(ctx, tui, "root", "new-user-intent", "D11_EXPLICIT_NEW_INTENT", "D11_NEW_INTENT_COMPLETED", 2);
        const current = JSON.stringify(bodyOf(ctx.requests[1]).messages);
        expect(current).toContain("D11_OLD_USER_INPUT"); expect(current).not.toContain("D11_FORBIDDEN_CANCELLED_TAIL");
        expect(stored(ctx.world, ctx.sessionIds.root!).filter((value) => value.includes("D11_OLD_USER_INPUT"))).toHaveLength(1);
        const resumed = readReplayStore(ctx.world);
        expect(resumed.threads.find((thread) => thread.id === ctx.sessionIds.root)?.work_state).toBe("idle");
        ctx.observe("explicit_stop_is_not_crash_recovery", { stopped, resumed });
      } });
  });

  it("D12 recovery explicitly reports unknown peripheral state when builtins are unavailable", async () => {
    await runReplayScenario({ id: "D12", title: "外围实例明确关闭时崩溃恢复如实Unknown，仍以新执行续跑",
      upstream: async () => sessionModel((body, index) => {
        if (index === 1) return { text: "D12_VISIBLE_BEFORE_CRASH", tail: " D12_FORBIDDEN_OLD_TAIL" };
        expect(index).toBe(2);
        const messages = JSON.stringify(body.messages);
        expect(messages).toContain("旧工具调用不会自动重放");
        expect(messages).toContain("外围任务归属与存活状态：未知");
        expect(messages).not.toContain("已确认当前没有仍存活的 Shell 任务");
        expect(messages).toContain("&quot;task_liveness&quot;:&quot;unknown&quot;");
        return { text: "D12_UNKNOWN_RECOVERY_COMPLETED" };
      }),
      runOptions: () => ({ gates: [{ identity: identity("root", "interrupted"),
        afterBytes: Buffer.byteLength(responsePrefix("D12_VISIBLE_BEFORE_CRASH")) }] }),
      async run(ctx) {
        const unavailable = { PERI_MCP_BUILTIN: "off" };
        let tui = await ctx.startTui("root", { label: "unavailable-before-crash", env: unavailable });
        ctx.identity.arm(ctx.sessionIds.root!, "interrupted"); await tui.prompt("D12_OLD_USER_INPUT");
        await ctx.run.waitReached(identity("root", "interrupted"));
        await tui.waitFor(ctx.checked((screen: string) => screen.includes("D12_VISIBLE_BEFORE_CRASH")), "partial output before crash");
        await tui.crash();
        await ctx.waitForCalls((snapshot) => snapshot.active === 0 && snapshot.calls[0]?.terminal === "client_abort");
        ctx.identity.finish(ctx.sessionIds.root!);
        expect(readReplayStore(ctx.world).threads.find((thread) => thread.id === ctx.sessionIds.root)?.work_state).toBe("working");
        ctx.identity.arm(ctx.sessionIds.root!, "unknown-recovery");
        tui = await ctx.startTui("root", { label: "unavailable-recovery", env: unavailable });
        const completed = await tui.waitFor(ctx.checked((screen: string) => screen.includes("D12_UNKNOWN_RECOVERY_COMPLETED")
          && screen.includes("Brewed for")), "recovery completes with truthful unknown liveness");
        expect(completed).not.toContain("D12_FORBIDDEN_OLD_TAIL");
        await ctx.waitForCalls((snapshot) => snapshot.active === 0 && snapshot.calls.length === 2);
        ctx.identity.finish(ctx.sessionIds.root!); await tui.save("unknown-recovery-complete");
        const recovered = readReplayStore(ctx.world);
        expect(recovered.threads.find((thread) => thread.id === ctx.sessionIds.root)?.work_state).toBe("idle");
        expect(stored(ctx.world, ctx.sessionIds.root!).filter((value) => value.includes("D12_OLD_USER_INPUT"))).toHaveLength(1);
        ctx.observe("unavailable_peripheral_state_is_not_empty", { recovered, request: bodyOf(ctx.requests[1]) });
      } });
  });
});
