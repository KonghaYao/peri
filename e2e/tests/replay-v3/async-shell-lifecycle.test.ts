import { readFile, unlink } from "node:fs/promises";
import path from "node:path";
import { expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { agentProvider, expectResult, resultText, text, tool, tools, type Reply } from "../replay-v2/agents-fixture.js";
import {
  ShellBindings, absent, assertExited, assertStartedOnce, configureShell, fileReady, finished,
  gatedCommand, processExists, programStatuses, releaseShell, requestText, resultFromBody, shellCall, startShellTui,
} from "./async-shell-fixture.js";

it("H05 Tasks panel cancels the selected Shell process group; same command can start a fresh task", { timeout: 150_000 }, async () => {
  const child = "( printf '%s\\n' \"$BASHPID\" > h05.child; for i in {1..6000}; do [ -f h05.release ] && break; sleep 0.01; done; [ -f h05.release ] && printf 'H05_CHILD_SIDE_EFFECT\\n' > h05.child-result ) & ";
  const command = "printf 'H05_BEFORE_CANCEL\\n'; printf 'H05_CANCEL_STDERR\\n' >&2; " + child + gatedCommand("h05", "printf 'H05_PARENT_SIDE_EFFECT\\n' > h05.result; wait");
  const specs = [{ id: "H05-old", alias: "old", command }, { id: "H05-new", alias: "new", command }];
  let binding: ShellBindings;
  await runReplayScenario({ id: "H05", title: "真实任务面板取消进程组，同名命令再次启动不受旧终态污染", configVersion: "shell-panel-cancel-1",
    configure: configureShell,
    upstream: async () => agentProvider((call, body): Reply => {
      if (["start", "restart"].includes(call.execution) && call.call === "reason:1") return tools(tool(call.execution === "start" ? "H05-old" : "H05-new", "Bash", { command, run_in_background: true }));
      if (["start", "restart"].includes(call.execution) && call.call === "reason:2") {
        expectResult(body, call.execution === "start" ? "H05-old" : "H05-new", "Background task started: mcp-");
        return text(call.execution === "start" ? "H05_CANCEL_TARGET_READY" : "H05_NEW_TASK_READY");
      }
      if (call.execution === "start" && call.call === "reason:3") {
        const result = resultFromBody(body, binding.task("old"))!;
        expect(result.state).toBe("cancelled");
        return tools(tool("H05-read-cancelled", "Read", { file_path: result.stdout! }));
      }
      if (call.execution === "start" && call.call === "reason:4") {
        expectResult(body, "H05-read-cancelled", "H05_BEFORE_CANCEL");
        return text("H05_OLD_CANCELLED");
      }
      if (call.execution === "restart" && call.call === "reason:3") {
        expect(resultFromBody(body, binding.task("new"))!.state).toBe("succeeded");
        return text("H05_NEW_COMPLETED");
      }
      throw new Error(`unexpected H05 ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new ShellBindings(ctx, specs); return { onRequest: binding.onRequest,
      gates: [{ identity: shellCall("reason:3"), afterBytes: 0 }] }; },
    async run(ctx) {
      const tui = await startShellTui(ctx);
      ctx.identity.arm(ctx.sessionIds.root!, "start"); await tui.prompt("Start H05 cancellable shell.");
      await finished(ctx, tui, "H05_CANCEL_TARGET_READY");
      const oldPids = [Number(await fileReady(ctx, "h05.pid")), Number(await fileReady(ctx, "h05.child"))];
      for (const pid of oldPids) expect(await processExists(pid)).toBe(true);
      await tui.command("tasks");
      const panel = await tui.waitFor(ctx.checked((screen: string) => screen.includes("Background") && screen.includes(binding.task("old"))), "specific live task selected in Tasks panel");
      const selected = panel.split("\n").find(line => line.includes(binding.task("old")))!;
      expect(selected).toMatch(/[›❯>▶]/u);
      await tui.save("selected-cancellation-target");
      await tui.key("Enter");
      await ctx.run.waitReached(shellCall("reason:3"), { timeout: 30_000 });
      await tui.waitFor(ctx.checked((screen: string) => screen.includes("Task cancelled:") && screen.includes(binding.task("old"))), "actual cancellation feedback names the stopped task");
      await tui.save("cancelled-task-feedback");
      ctx.run.release(shellCall("reason:3"));
      await finished(ctx, tui, "H05_OLD_CANCELLED");
      await assertExited(oldPids);
      expect(binding.output("old").state).toBe("cancelled");
      expect(await readFile(binding.output("old").stdout!, "utf8")).toBe("H05_BEFORE_CANCEL\n");
      expect(await readFile(binding.output("old").stderr!, "utf8")).toBe("H05_CANCEL_STDERR\n");
      const cancelledRead = ctx.requests.find(call => call.context.execution === "start" && call.context.call === "reason:4")!;
      expect(resultText(JSON.parse(cancelledRead.request.body.toString()), "H05-read-cancelled")).toContain("H05_BEFORE_CANCEL");
      ctx.identity.finish(ctx.sessionIds.root!);
      await releaseShell(ctx, "h05");
      await absent(ctx, "h05.result"); await absent(ctx, "h05.child-result");
      await unlink(path.join(ctx.world.work, "h05.release"));
      await unlink(path.join(ctx.world.work, "h05.pid")); await unlink(path.join(ctx.world.work, "h05.child"));
      ctx.identity.arm(ctx.sessionIds.root!, "restart"); await tui.prompt("Start the exact same H05 command again.");
      await finished(ctx, tui, "H05_NEW_TASK_READY");
      const newPids = [Number(await fileReady(ctx, "h05.pid")), Number(await fileReady(ctx, "h05.child"))];
      expect(newPids.some(pid => oldPids.includes(pid))).toBe(false);
      expect(binding.task("new")).not.toBe(binding.task("old"));
      expect(await readFile(path.join(ctx.world.work, "h05.starts"), "utf8")).toBe("started\nstarted\n");
      const statuses = await programStatuses(tui, "old-cancelled-new-running"); expect(statuses.at(-1)).toBe("idle");
      await releaseShell(ctx, "h05"); await finished(ctx, tui, "H05_NEW_COMPLETED");
      ctx.identity.finish(ctx.sessionIds.root!); await assertExited(newPids);
      expect(binding.output("new").state).toBe("succeeded");
      expect(await fileReady(ctx, "h05.result")).toBe("H05_PARENT_SIDE_EFFECT\n");
      expect(await fileReady(ctx, "h05.child-result")).toBe("H05_CHILD_SIDE_EFFECT\n");
      expect(ctx.requests.filter(call => call.context.execution === "start" && call.context.call === "reason:3")).toHaveLength(1);
      ctx.observe("selected-cancel-restart", { oldTask: binding.task("old"), newTask: binding.task("new"), oldPids, newPids, selected });
      await tui.save("fresh-task-completed");
    },
  });
});

it("H06 two parallel Shells settle in reverse order with the correct actual log and task", { timeout: 120_000 }, async () => {
  const specs = ["a", "b"].map(alias => ({ id: `H06-${alias}`, alias, command: gatedCommand(`h06-${alias}`, `printf 'H06_${alias.toUpperCase()}_OUTPUT\\n'; printf '${alias}\\n' > h06-${alias}.result`) }));
  let binding: ShellBindings;
  await runReplayScenario({ id: "H06", title: "同会话并行 Shell 反序完成，任务身份、真实 Read 和通知不串线", configVersion: "shell-reverse-1",
    configure: configureShell,
    upstream: async () => agentProvider((call, body): Reply => {
      if (call.execution === "start" && call.call === "reason:1") return tools(...specs.map(spec => tool(spec.id, "Bash", { command: spec.command, run_in_background: true })));
      if (call.execution === "start" && call.call === "reason:2") {
        for (const spec of specs) expectResult(body, spec.id, "Background task started: mcp-");
        return text("H06_TWO_RUNNING");
      }
      const alias = ["reason:3", "reason:4"].includes(call.call) ? "b" : ["reason:5", "reason:6"].includes(call.call) ? "a" : undefined;
      if (alias && ["reason:3", "reason:5"].includes(call.call)) {
        const result = resultFromBody(body, binding.task(alias))!; expect(result.state).toBe("succeeded");
        return tools(tool(`H06-read-${alias}`, "Read", { file_path: result.stdout! }));
      }
      if (alias && ["reason:4", "reason:6"].includes(call.call)) {
        expectResult(body, `H06-read-${alias}`, `H06_${alias.toUpperCase()}_OUTPUT`);
        return text(`H06_${alias.toUpperCase()}_VERIFIED`);
      }
      throw new Error(`unexpected H06 ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new ShellBindings(ctx, specs); return { onRequest: binding.onRequest }; },
    async run(ctx) {
      const tui = await startShellTui(ctx); ctx.identity.arm(ctx.sessionIds.root!, "start"); await tui.prompt("Run both H06 shells in parallel.");
      await finished(ctx, tui, "H06_TWO_RUNNING");
      const pids = [Number(await fileReady(ctx, "h06-a.pid")), Number(await fileReady(ctx, "h06-b.pid"))];
      expect(new Set(binding.tasks.values()).size).toBe(2); expect(new Set(pids).size).toBe(2);
      for (const alias of ["b", "a"]) {
        await releaseShell(ctx, `h06-${alias}`);
        await finished(ctx, tui, `H06_${alias.toUpperCase()}_VERIFIED`);
        expect(await fileReady(ctx, `h06-${alias}.result`)).toBe(`${alias}\n`);
        expect(binding.output(alias).state).toBe("succeeded");
        expect(await readFile(binding.output(alias).stdout!, "utf8")).toBe(`H06_${alias.toUpperCase()}_OUTPUT\n`);
        const readRequest = ctx.requests.find(call => call.context.execution === "start" && call.context.call === (alias === "b" ? "reason:4" : "reason:6"))!;
        expect(resultText(JSON.parse(readRequest.request.body.toString()), `H06-read-${alias}`)).toContain(`H06_${alias.toUpperCase()}_OUTPUT`);
        await assertStartedOnce(ctx, `h06-${alias}`);
        if (alias === "b") { expect(await processExists(pids[0]!)).toBe(true); await absent(ctx, "h06-a.result"); }
      }
      await assertExited(pids);
      expect(binding.output("a").stdout).not.toBe(binding.output("b").stdout);
      ctx.identity.finish(ctx.sessionIds.root!);
      const completions = ctx.requests.filter(call => ["reason:3", "reason:5"].includes(call.context.call)).map(call => call.context.call);
      expect(completions).toEqual(["reason:3", "reason:5"]);
      ctx.observe("reverse-process-completion", { pids, tasks: Object.fromEntries(binding.tasks), completions });
      await tui.save("both-shells-completed-in-reverse-order");
    },
  });
});

it("H09 positive background timeout kills the process and reports timed_out without late writes", { timeout: 120_000 }, async () => {
  const command = gatedCommand("h09", "printf 'H09_FORBIDDEN_LATE_WRITE\\n' > h09.late");
  const spec = { id: "H09-bash", alias: "h09", command }; let binding: ShellBindings;
  await runReplayScenario({ id: "H09", title: "后台 Shell 正超时产生真实 timed_out，并阻止超时后的迟写", configVersion: "shell-timeout-1",
    configure: configureShell,
    upstream: async () => agentProvider((call, body): Reply => {
      if (call.execution === "start" && call.call === "reason:1") return tools(tool(spec.id, "Bash", { command, run_in_background: true, timeout: 3000 }));
      if (call.execution === "start" && call.call === "reason:2") { expectResult(body, spec.id, "Background task started:"); return text("H09_TIMEOUT_ARMED"); }
      if (call.execution === "start" && call.call === "reason:3") { expect(resultFromBody(body, binding.task("h09"))!.state).toBe("timed_out"); return text("H09_TIMEOUT_VERIFIED"); }
      throw new Error(`unexpected H09 ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new ShellBindings(ctx, [spec]); return { onRequest: binding.onRequest }; },
    async run(ctx) {
      const tui = await startShellTui(ctx); ctx.identity.arm(ctx.sessionIds.root!, "start");
      await tui.prompt("Run H09 with a real background deadline.");
      const pid = Number(await fileReady(ctx, "h09.pid")); expect(await processExists(pid)).toBe(true);
      await finished(ctx, tui, "H09_TIMEOUT_VERIFIED"); ctx.identity.finish(ctx.sessionIds.root!);
      await assertExited([pid]); await releaseShell(ctx, "h09"); await absent(ctx, "h09.late");
      await assertStartedOnce(ctx, "h09");
      const output = binding.output("h09"); expect(output.state).toBe("timed_out"); expect(output.exitCode).toBe("unknown");
      expect(requestText(JSON.parse(ctx.requests.at(-1)!.request.body.toString()))).toContain(`task_id: ${binding.task("h09")}`);
      ctx.observe("real-timeout", { pid, state: output.state, noLateWrite: true }); await tui.save("real-background-timeout");
    },
  });
});
