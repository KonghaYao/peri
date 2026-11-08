import { readFile } from "node:fs/promises";
import path from "node:path";
import { expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { agentProvider, expectResult, resultText, text, tool, tools, type Reply } from "../replay-v2/agents-fixture.js";
import { shellAcp } from "./async-shell-acp.js";
import {
  ShellBindings, absent, assertExited, assertStartedOnce, configureShell, fileReady,
  gatedCommand, processExists, releaseShell, requestText, resultFromBody, shellCall, startShellTui,
} from "./async-shell-fixture.js";

it("H07 another session cannot cancel an owned shell; both owners still finish and restore isolated TUI histories", { timeout: 150_000 }, async () => {
  const specs = ["A", "B"].map(alias => ({ id: `H07-${alias}`, alias, command: gatedCommand(`h07-${alias}`, `printf 'H07_${alias}_REAL_OUTPUT\\n'; printf '${alias}\\n' > h07-${alias}.result`) }));
  let binding: ShellBindings;
  await runReplayScenario({ id: "H07", title: "跨会话取消被明确拒绝，原任务均可完成且真实 TUI 历史隔离", sessionNames: ["A", "B"], configVersion: "shell-owner-isolation-1",
    configure: configureShell,
    upstream: async () => agentProvider((call, body): Reply => {
      const alias = call.owner; const other = alias === "A" ? "B" : "A"; const spec = specs.find(item => item.alias === alias)!;
      expect(requestText(body)).not.toContain(`H07_${other}_REAL_OUTPUT`);
      if (call.execution === "start" && call.call === "reason:1") return tools(tool(spec.id, "Bash", { command: spec.command, run_in_background: true }));
      if (call.execution === "start" && call.call === "reason:2") { expectResult(body, spec.id, "Background task started:"); return text(`H07_${alias}_WAITING`); }
      if (call.execution === "start" && call.call === "reason:3") {
        const result = resultFromBody(body, binding.task(alias))!; expect(result.state).toBe("succeeded");
        expect(requestText(body)).not.toContain(binding.task(other));
        return tools(tool(`H07-read-${alias}`, "Read", { file_path: result.stdout! }));
      }
      if (call.execution === "start" && call.call === "reason:4") {
        expectResult(body, `H07-read-${alias}`, `H07_${alias}_REAL_OUTPUT`); return text(`H07_${alias}_VERIFIED`);
      }
      throw new Error(`unexpected H07 ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new ShellBindings(ctx, specs); return { onRequest: binding.onRequest }; },
    async run(ctx) {
      const acp = await shellAcp(ctx); let closed = false;
      try {
        for (const alias of ["A", "B"]) { await acp.enqueue(alias, "start", `Start session ${alias}'s H07 shell.`); await acp.suspended(alias, `H07_${alias}_WAITING`); }
        const pids = await Promise.all(["A", "B"].map(async alias => Number(await fileReady(ctx, `h07-${alias}.pid`))));
        for (const [alias, other] of [["A", "B"], ["B", "A"]] as const) {
          const snapshot = await acp.tasks(alias);
          expect(snapshot.tasks.map((task: any) => task.task_id)).toEqual([binding.task(alias)]);
          expect(JSON.stringify(snapshot)).not.toContain(binding.task(other));
          const rejected = await acp.peer.call("session/cancel-bg-task", { sessionId: ctx.sessionIds[alias], taskId: binding.task(other) });
          expect(rejected.result).toBeUndefined(); expect(rejected.error?.code).toBe(-32603);
          ctx.observe(`wrong-owner-${alias}-cancel`, rejected);
        }
        for (const pid of pids) expect(await processExists(pid)).toBe(true);
        for (const alias of ["A", "B"]) await absent(ctx, `h07-${alias}.result`);
        const completionOrder = ctx.mode === "record" ? ["B", "A"] : ["A", "B"];
        for (const alias of completionOrder) {
          await releaseShell(ctx, `h07-${alias}`);
          await acp.done(alias, `H07_${alias}_VERIFIED`);
          expect(await fileReady(ctx, `h07-${alias}.result`)).toBe(`${alias}\n`); await assertStartedOnce(ctx, `h07-${alias}`);
          expect(binding.output(alias).state).toBe("succeeded");
          const actualRead = ctx.requests.find(call => call.context.owner === alias && call.context.call === "reason:4")!;
          expect(resultText(JSON.parse(actualRead.request.body.toString()), `H07-read-${alias}`)).toContain(`H07_${alias}_REAL_OUTPUT`);
        }
        await assertExited(pids);
        for (const [alias, other] of [["A", "B"], ["B", "A"]] as const) {
          expect(acp.text(alias)).toContain(`H07_${alias}_VERIFIED`); expect(acp.text(alias)).not.toContain(`H07_${other}_VERIFIED`);
        }
        await acp.close(); closed = true;
        for (const [alias, other] of [["A", "B"], ["B", "A"]] as const) {
          const tui = await startShellTui(ctx, alias);
          const screen = await tui.waitFor(ctx.checked((screen: string) => screen.includes(`H07_${alias}_VERIFIED`)), "restored owner's real TUI history");
          expect(screen).not.toContain(`H07_${other}_VERIFIED`); await tui.save("owner-isolated-shell-history"); await tui.stop();
        }
        ctx.observe("cross-session-shell-isolation", { completionOrder, pids, tasks: Object.fromEntries(binding.tasks) });
      } finally { await acp.save("H07"); if (!closed) await acp.closeTransport(); }
    },
  });
});

it("H08 shell completion while the next user turn is busy joins that turn once without a competing execution", { timeout: 150_000 }, async () => {
  const command = gatedCommand("h08", "printf 'H08_BG_ACTUAL\\n' > h08.result");
  const spec = { id: "H08-background", alias: "h08", command }; let binding: ShellBindings;
  await runReplayScenario({ id: "H08", title: "下一用户回合进行中时 Shell 完成，通知恰好一次合入活动执行", configVersion: "shell-busy-turn-1",
    configure: configureShell,
    upstream: async () => agentProvider((call, body): Reply => {
      if (call.execution === "start" && call.call === "reason:1") return tools(tool(spec.id, "Bash", { command, run_in_background: true }));
      if (call.execution === "start" && call.call === "reason:2") { expectResult(body, spec.id, "Background task started:"); return text("H08_PARENT_IDLE"); }
      if (call.execution === "start" && call.call === "reason:3") {
        expect(resultFromBody(body, binding.task("h08"))).toBeUndefined();
        return tools(tool("H08-read", "Read", { file_path: "h08.result" }));
      }
      if (call.execution === "start" && call.call === "reason:4") {
        expectResult(body, "H08-read", "H08_BG_ACTUAL"); expect(resultFromBody(body, binding.task("h08"))!.state).toBe("succeeded");
        expect(requestText(body).split(`task_id: ${binding.task("h08")}\n`)).toHaveLength(2);
        return text("H08_BUSY_TURN_CONSUMED_SHELL_RESULT");
      }
      throw new Error(`unexpected H08 competing execution ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new ShellBindings(ctx, [spec]); return { onRequest: binding.onRequest, gates: [{ identity: shellCall("reason:3", "start"), afterBytes: 0 }] }; },
    async run(ctx) {
      const acp = await shellAcp(ctx); let closed = false;
      try {
        await acp.enqueue("root", "start", "Start H08 background process."); await acp.suspended("root", "H08_PARENT_IDLE");
        const pid = Number(await fileReady(ctx, "h08.pid")); expect(await processExists(pid)).toBe(true);
        await acp.enqueue("root", "start", "Read H08 result during my next turn.", true);
        await ctx.run.waitReached(shellCall("reason:3", "start"), { timeout: 30_000 });
        await releaseShell(ctx, "h08");
        await expect.poll(async () => (await acp.tasks("root")).tasks.find((task: any) => task.task_id === binding.task("h08"))?.status,
          { timeout: 30_000, interval: 50 }).toBe("completed");
        await assertExited([pid]); expect(await fileReady(ctx, "h08.result")).toBe("H08_BG_ACTUAL\n");
        expect(ctx.proxy.snapshot(ctx.runId).calls).toHaveLength(3);
        expect(ctx.proxy.snapshot(ctx.runId).calls.at(-1).writtenBytes).toBe(0);
        ctx.run.release(shellCall("reason:3", "start")); await acp.done("root", "H08_BUSY_TURN_CONSUMED_SHELL_RESULT");
        await ctx.waitForCalls(snapshot => snapshot.active === 0 && snapshot.calls.length === 4);
        expect(new Set(ctx.requests.map(call => call.context.execution))).toEqual(new Set(["start"]));
        const beforeCompletion = JSON.parse(ctx.requests.find(call => call.context.call === "reason:3")!.request.body.toString());
        const afterCompletion = JSON.parse(ctx.requests.find(call => call.context.call === "reason:4")!.request.body.toString());
        expect(resultFromBody(beforeCompletion, binding.task("h08"))).toBeUndefined();
        expect(resultFromBody(afterCompletion, binding.task("h08"))!.state).toBe("succeeded");
        expect(requestText(afterCompletion).split(`task_id: ${binding.task("h08")}\n`)).toHaveLength(2);
        expect(resultText(afterCompletion, "H08-read")).toContain("H08_BG_ACTUAL");
        await assertStartedOnce(ctx, "h08"); await acp.close(); closed = true;
        const tui = await startShellTui(ctx); await tui.waitFor(ctx.checked((screen: string) => screen.includes("H08_BUSY_TURN_CONSUMED_SHELL_RESULT")), "busy turn final result restored");
        await tui.save("busy-turn-single-delivery");
        ctx.observe("busy-turn-no-extra-execution", { task: binding.task("h08"), pid, calls: ctx.requests.map(call => call.context) });
      } finally { await acp.save("H08"); if (!closed) await acp.closeTransport(); }
    },
  });
});

it("H10 closing the owning session reaps Shells; reloading its store does not resurrect live work", { timeout: 150_000 }, async () => {
  const command = gatedCommand("h10", "printf 'H10_FORBIDDEN_AFTER_CLOSE\\n' > h10.late");
  const spec = { id: "H10-bash", alias: "h10", command }; let binding: ShellBindings;
  await runReplayScenario({ id: "H10", title: "关闭会话清理后台进程，重载独立 session store 不复活旧 Shell", configVersion: "shell-close-restart-1",
    configure: configureShell,
    upstream: async () => agentProvider((call, body): Reply => {
      if (call.execution === "start" && call.call === "reason:1") return tools(tool(spec.id, "Bash", { command, run_in_background: true }));
      if (call.execution === "start" && call.call === "reason:2") { expectResult(body, spec.id, "Background task started:"); return text("H10_HISTORY_BEFORE_CLOSE"); }
      throw new Error(`unexpected H10 resurrection or wake ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new ShellBindings(ctx, [spec]); return { onRequest: binding.onRequest }; },
    async run(ctx) {
      const acp = await shellAcp(ctx); let closed = false;
      try {
        await acp.enqueue("root", "start", "Start H10 shell, then close the owning session."); await acp.suspended("root", "H10_HISTORY_BEFORE_CLOSE");
        const pid = Number(await fileReady(ctx, "h10.pid")); expect(await processExists(pid)).toBe(true);
        const before = await acp.tasks("root"); expect(before.tasks.map((task: any) => task.task_id)).toEqual([binding.task("h10")]);
        const closeReply = await acp.peer.call("session/close", { sessionId: ctx.sessionIds.root }); expect(closeReply.error).toBeUndefined(); ctx.identity.finish(ctx.sessionIds.root!);
        // Check the owner close barrier before fixture shutdown can kill any process itself.
        await assertExited([pid]); await releaseShell(ctx, "h10"); await absent(ctx, "h10.late");
        expect(await acp.closeTransport()).toBe(0); closed = true;
        const reopened = await shellAcp(ctx); let reopenedClosed = false;
        try {
          const after = await reopened.tasks("root"); expect(after.tasks).toEqual([]);
          expect(await processExists(pid)).toBe(false); await assertStartedOnce(ctx, "h10");
          await reopened.close(); reopenedClosed = true;
          ctx.observe("session-close-barrier", { pid, task: binding.task("h10"), before, after, closeReply });
        } finally { await reopened.save("H10-reopened"); if (!reopenedClosed) await reopened.closeTransport(); }
        const tui = await startShellTui(ctx); await tui.waitFor(ctx.checked((screen: string) => screen.includes("H10_HISTORY_BEFORE_CLOSE")), "pre-close shell history restored");
        await tui.command("tasks");
        const panel = await tui.waitFor(ctx.checked((screen: string) => screen.includes("No active tasks")), "empty live Tasks panel after reopening");
        expect(panel.split("\n").filter(line => line.includes(binding.task("h10")) && /running/iu.test(line))).toEqual([]);
        await tui.save("reopened-no-live-shell");
        await tui.key("Escape");
        expect(ctx.requests).toHaveLength(2); expect(await readFile(path.join(ctx.world.work, "h10.starts"), "utf8")).toBe("started\n");
      } finally { await acp.save("H10"); if (!closed) await acp.closeTransport(); }
    },
  });
});
