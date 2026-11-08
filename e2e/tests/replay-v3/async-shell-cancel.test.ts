import { readFile } from "node:fs/promises";
import { expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { agentProvider, expectResult, resultText, text, tool, tools, type Reply } from "../replay-v2/agents-fixture.js";
import { shellAcp } from "./async-shell-acp.js";
import { ShellBindings, absent, assertExited, configureShell, fileReady, gatedCommand, releaseShell, resultFromBody, shellCall, startShellTui } from "./async-shell-fixture.js";

it("H12 ACP cancellation preserves a cancelled snapshot and one cancellation event with readable process output", { timeout: 120_000 }, async () => {
  const command = "printf 'H12_CAPTURED_BEFORE_CANCEL\\n'; " + gatedCommand("h12", "printf 'forbidden\\n' > h12.late");
  const spec = { id: "H12-bash", alias: "h12", command }; let binding: ShellBindings;
  await runReplayScenario({ id: "H12", title: "ACP 取消保留 cancelled 快照、单次取消事件与真实输出证据", configVersion: "shell-cancel-projection-1",
    configure: configureShell,
    upstream: async () => agentProvider((call, body): Reply => {
      if (call.call === "reason:1") return tools(tool(spec.id, "Bash", { command, run_in_background: true }));
      if (call.call === "reason:2") { expectResult(body, spec.id, "Background task started:"); return text("H12_SHELL_WAITING"); }
      if (call.call === "reason:3") {
        const result = resultFromBody(body, binding.task("h12"))!; expect(result.state).toBe("cancelled");
        return tools(tool("H12-read", "Read", { file_path: result.stdout! }));
      }
      if (call.call === "reason:4") { expectResult(body, "H12-read", "H12_CAPTURED_BEFORE_CANCEL"); return text("H12_CANCELLED_OUTPUT_VERIFIED"); }
      throw new Error(`unexpected H12 ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new ShellBindings(ctx, [spec]); return { onRequest: binding.onRequest, gates: [{ identity: shellCall("reason:3"), afterBytes: 0 }] }; },
    async run(ctx) {
      const acp = await shellAcp(ctx); let closed = false;
      try {
        await acp.enqueue("root", "start", "Start H12, then cancel through ACP."); await acp.suspended("root", "H12_SHELL_WAITING");
        const pid = Number(await fileReady(ctx, "h12.pid"));
        const cancelled = await acp.peer.call("session/cancel-bg-task", { sessionId: ctx.sessionIds.root, taskId: binding.task("h12") });
        expect(cancelled.error).toBeUndefined(); expect(cancelled.result.success).toBe(true);
        await ctx.run.waitReached(shellCall("reason:3"), { timeout: 30_000 });
        await expect.poll(async () => (await acp.tasks("root")).tasks.find((task: any) => task.task_id === binding.task("h12"))?.status,
          { timeout: 20_000, interval: 50 }).toBe("cancelled");
        const terminalEvents = () => acp.notifications.filter(message => message.method === "peri/unstable_event" && message.params?.data?.task_id === binding.task("h12")
          && ["bg-task-completed", "bg-task-cancelled"].includes(message.params.event));
        await expect.poll(() => terminalEvents().map(message => message.params.event), { timeout: 20_000 }).toEqual(["bg-task-cancelled"]);
        await assertExited([pid]); await releaseShell(ctx, "h12"); await absent(ctx, "h12.late");
        expect(binding.output("h12").state).toBe("cancelled");
        expect(await readFile(binding.output("h12").stdout!, "utf8")).toBe("H12_CAPTURED_BEFORE_CANCEL\n");
        ctx.observe("cancelled-projection-and-wire", { pid, cancelled, snapshot: await acp.tasks("root"), terminalEvents: terminalEvents() });
        ctx.run.release(shellCall("reason:3")); await acp.done("root", "H12_CANCELLED_OUTPUT_VERIFIED");
        const last = JSON.parse(ctx.requests.find(call => call.context.call === "reason:4")!.request.body.toString());
        expect(resultText(last, "H12-read")).toContain("H12_CAPTURED_BEFORE_CANCEL");
        expect(terminalEvents()).toHaveLength(1);
        await acp.close(); closed = true;
        const tui = await startShellTui(ctx); await tui.waitFor(ctx.checked((screen: string) => screen.includes("H12_CANCELLED_OUTPUT_VERIFIED")), "cancelled result restores into actual TUI history");
        await tui.save("cancelled-output-history");
      } finally { await acp.save("H12"); if (!closed) await acp.closeTransport(); }
    },
  });
});
