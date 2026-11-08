import { readFile } from "node:fs/promises";
import path from "node:path";
import { expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { agentProvider, expectResult, text, tools, type Reply } from "../replay-v2/agents-fixture.js";
import { assertExited } from "./async-shell-fixture.js";
import {
  MonitorBindings, bodyText, closeMonitorPanel, configureMonitor, discoverMonitor, jsonResult, monitorCall, monitorCommand, monitorDone,
  monitorFile, monitorGate, monitorTool, noMonitorFile, portableMonitorTools, requestBody, startMonitorTui,
} from "./monitor-fixture.js";

for (const entry of [
  { id: "M01", title: "monitor 运行中增量告警、真实 Tasks 面板与终态合并未报告命中", secondAlert: false },
  { id: "M09", title: "monitor 真实 30 秒合并窗口后再次唤醒，终态不重复已报告命中", secondAlert: true },
] as const) it(`${entry.id} ${entry.secondAlert ? "[slow] " : ""}${entry.title}`, { timeout: 150_000 }, async () => {
  const alias = entry.id.toLowerCase(), spec = { id: `${entry.id}-start`, alias, command: monitorCommand(alias, "incremental") };
  let binding: MonitorBindings;
  await runReplayScenario({ id: entry.id, title: entry.title, configVersion: `monitor-alert-${entry.id}-2`, configure: configureMonitor,
    portableTools: portableMonitorTools,
    upstream: async () => agentProvider((call, body): Reply => {
      if (call.execution === "start" && call.call === "reason:1") return tools(discoverMonitor(entry.id));
      if (call.execution === "start" && call.call === "reason:2") return tools(monitorTool(spec.id, "monitor_start", {
        command: spec.command, label: `${entry.id} live monitor`, alert: `${entry.id}_ALERT_`,
      }));
      if (call.execution === "start" && call.call === "reason:3") return text(`${entry.id}_PARENT_IDLE`);
      if (call.execution === "start" && call.call === "reason:4") {
        expect(bodyText(body)).toContain("matched its alert 1 time(s); the latest matching event is seq 1");
        expect(bodyText(body)).not.toContain(`${entry.id}_ALERT_1`);
        return tools(monitorTool(`${entry.id}-read-first`, "monitor_read", { task_id: binding.raw(alias), after_seq: 0 }));
      }
      if (call.execution === "start" && call.call === "reason:5") {
        expect(jsonResult(body, `${entry.id}-read-first`).events.map((event: any) => event.text)).toEqual([`${entry.id}_ALERT_1`]);
        return text(`${entry.id}_FIRST_ALERT_READ`);
      }
      if (entry.secondAlert && call.execution === "start" && call.call === "reason:6") {
        expect(bodyText(body)).toContain("matched its alert 3 time(s); the latest matching event is seq 3");
        return tools(monitorTool(`${entry.id}-read-second`, "monitor_read", { task_id: binding.raw(alias), after_seq: 1 }));
      }
      if (entry.secondAlert && call.execution === "start" && call.call === "reason:7") return text(`${entry.id}_SECOND_ALERT_READ`);
      if (call.execution === "start" && call.call === (entry.secondAlert ? "reason:8" : "reason:6")) {
        const message = bodyText(body), terminal = message.slice(message.lastIndexOf("MCP task on monitor finished"));
        expect(terminal).toContain("status Completed");
        if (entry.secondAlert) expect(terminal).not.toContain("hit(s) were not reported");
        else expect(terminal).toContain("2 hit(s) were not reported before the task ended");
        return tools(monitorTool(`${entry.id}-read-terminal`, "monitor_read", { task_id: binding.raw(alias), after_seq: entry.secondAlert ? 3 : 1 }));
      }
      if (call.execution === "start" && call.call === (entry.secondAlert ? "reason:9" : "reason:7")) return text(`${entry.id}_LIFECYCLE_VERIFIED`);
      throw new Error(`unexpected ${entry.id} ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new MonitorBindings(ctx, [spec]); return { onRequest: binding.onRequest, requestRules: binding.requestRules }; },
    async run(ctx) {
      const tui = await startMonitorTui(ctx);
      ctx.identity.arm(ctx.sessionIds.root!, "start"); await tui.prompt(`Start ${entry.id} monitor and process its real notifications.`);
      await monitorDone(ctx, tui, `${entry.id}_PARENT_IDLE`); await monitorFile(ctx, `${alias}.pid`);
      await tui.command("tasks");
      const panel = await tui.waitFor(ctx.checked((screen: string) => screen.includes("Background") && screen.includes("monitor")), "real live monitor Tasks row");
      expect(panel).toMatch(/running|Running|Working|working/u); await tui.save("monitor-live-task-panel"); await closeMonitorPanel(ctx, tui);
      // A monitor wake resumes the suspended execution; it is not a new user turn.
      await monitorGate(ctx, alias); await monitorDone(ctx, tui, `${entry.id}_FIRST_ALERT_READ`);
      const firstAt = Date.now(); expect(binding.results.get(`${entry.id}-read-first`).status).toBe("working");
      expect(binding.results.get(`${entry.id}-read-first`).nextSeq).toBe(1);
      if (entry.secondAlert) {
        await monitorGate(ctx, alias, ".second");
        await monitorDone(ctx, tui, `${entry.id}_SECOND_ALERT_READ`);
        const waited = Date.now() - firstAt; expect(waited).toBeGreaterThanOrEqual(27_000);
        expect(binding.results.get(`${entry.id}-read-second`).events.map((event: any) => event.seq)).toEqual([2, 3]);
        expect(binding.results.get(`${entry.id}-read-second`).status).toBe("working");
        ctx.observe("actual-monitor-merge-window", { waitedMs: waited, cumulativeHits: 3 });
      }
      if (!entry.secondAlert) { await monitorGate(ctx, alias, ".second"); await monitorFile(ctx, `${alias}.second-done`); }
      await monitorGate(ctx, alias, ".finish"); await monitorDone(ctx, tui, `${entry.id}_LIFECYCLE_VERIFIED`);
      ctx.identity.finish(ctx.sessionIds.root!);
      const terminal = binding.results.get(`${entry.id}-read-terminal`);
      expect(terminal.status).toBe("completed"); expect(terminal.nextSeq).toBe(3);
      expect(terminal.events.map((event: any) => event.seq)).toEqual(entry.secondAlert ? [] : [2, 3]);
      expect([...binding.events.get(alias)!.keys()].sort()).toEqual([1, 2, 3]);
      const executions = ctx.requests.filter((request) => request.context.purpose === "reason" && request.context.call === "reason:1").map((request) => request.context.execution);
      expect(executions).toEqual(["start"]);
      expect(ctx.requests.filter((request) => request.context.purpose === "reason")).toHaveLength(entry.secondAlert ? 9 : 7);
      expect(await monitorFile(ctx, `${alias}.starts`)).toBe("started\n");
      ctx.observe("monitor-notification-oracle", { executions, raw: binding.raw(alias), capturedSeq: [1, 2, 3], terminalStatus: terminal.status });
      await tui.save("monitor-notification-lifecycle");
    },
  });
});

it("M07 monitor deadline cancels its real process and wakes the owner with failure", { timeout: 120_000 }, async () => {
  const spec = { id: "M07-start", alias: "m07", command: monitorCommand("m07", "deadline") }; let binding: MonitorBindings;
  await runReplayScenario({ id: "M07", title: "monitor deadline 杀掉真实进程，保留已捕获事件并失败唤醒原会话", configVersion: "monitor-deadline-2",
    configure: configureMonitor, portableTools: portableMonitorTools,
    upstream: async () => agentProvider((call, body): Reply => {
      if (call.execution === "start" && call.call === "reason:1") return tools(discoverMonitor("M07"));
      if (call.execution === "start" && call.call === "reason:2") return tools(monitorTool(spec.id, "monitor_start", { command: spec.command, deadline_ms: 5_000, label: "M07 deadline" }));
      if (call.execution === "start" && call.call === "reason:3") return text("M07_PARENT_IDLE");
      if (call.execution === "start" && call.call === "reason:4") {
        expect(bodyText(body)).toContain("status Failed"); expect(bodyText(body)).toMatch(/deadline/iu);
        return tools(monitorTool("M07-read", "monitor_read", { task_id: binding.raw("m07") }));
      }
      if (call.execution === "start" && call.call === "reason:5") return tools(monitorTool("M07-list", "monitor_list", { include_ended: true }));
      if (call.execution === "start" && call.call === "reason:6") return text("M07_DEADLINE_VERIFIED");
      throw new Error(`unexpected M07 ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new MonitorBindings(ctx, [spec]); return { onRequest: binding.onRequest, requestRules: binding.requestRules }; },
    async run(ctx) {
      const tui = await startMonitorTui(ctx); ctx.identity.arm(ctx.sessionIds.root!, "start"); await tui.prompt("Run M07 short deadline monitor.");
      await monitorDone(ctx, tui, "M07_PARENT_IDLE"); const pid = Number(await monitorFile(ctx, "m07.pid"));
      // Wait for the same execution to resume on the actual terminal reminder.
      const screen = await monitorDone(ctx, tui, "M07_DEADLINE_VERIFIED"); expect(screen).toMatch(/fail|deadline/iu);
      ctx.identity.finish(ctx.sessionIds.root!); await assertExited([pid]);
      const read = binding.results.get("M07-read"), snapshot = binding.results.get("M07-list").tasks[0];
      expect(read.status).toBe("failed"); expect(read.events.map((event: any) => event.text)).toEqual(["M07_DEADLINE_READY"]);
      expect(snapshot.taskId).toBe(binding.raw("m07")); expect(snapshot.status).toBe("failed"); expect(snapshot.cleanup.processTerminated).toBe(true);
      // The implementation samples startedAt and deadlineAt separately; a millisecond boundary is valid.
      expect(Date.parse(snapshot.deadlineAt) - Date.parse(snapshot.startedAt)).toBeGreaterThanOrEqual(5_000);
      expect(Date.parse(snapshot.finishedAt)).toBeGreaterThanOrEqual(Date.parse(snapshot.deadlineAt));
      await noMonitorFile(ctx, "m07.ended"); ctx.observe("monitor-deadline-oracle", { pid, status: read.status, cleanup: snapshot.cleanup, deadlineMs: 5_000 });
      await tui.save("monitor-deadline-failed");
    },
  });
});

it("M08 invalid monitor regex has no side effects and the same session can recover", { timeout: 120_000 }, async () => {
  const spec = { id: "M08-start", alias: "m08", command: monitorCommand("m08", "plain") }; let binding: MonitorBindings;
  await runReplayScenario({ id: "M08", title: "monitor 非法 regex 启动失败无进程副作用，同会话随后恢复成功", configVersion: "monitor-regex-2",
    configure: configureMonitor, portableTools: portableMonitorTools,
    upstream: async () => agentProvider((call, body): Reply => {
      if (call.execution === "start" && call.call === "reason:1") return tools(discoverMonitor("M08"));
      if (call.execution === "start" && call.call === "reason:2") return tools(monitorTool("M08-invalid", "monitor_start", { command: "printf forbidden > m08.forbidden", alert: "[" }));
      if (call.execution === "start" && call.call === "reason:3") {
        expectResult(body, "M08-invalid", "regex", true); return tools(monitorTool("M08-list-empty", "monitor_list", { include_ended: true }));
      }
      if (call.execution === "start" && call.call === "reason:4") {
        expect(jsonResult(body, "M08-list-empty").tasks).toEqual([]); return tools(monitorTool(spec.id, "monitor_start", { command: spec.command, label: "M08 recovered" }));
      }
      if (call.execution === "start" && call.call === "reason:5") return text("M08_RECOVERED_PARENT_IDLE");
      if (call.execution === "start" && call.call === "reason:6") return tools(monitorTool("M08-read", "monitor_read", { task_id: binding.raw("m08") }));
      if (call.execution === "start" && call.call === "reason:7") return text("M08_RECOVERY_VERIFIED");
      throw new Error(`unexpected M08 ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new MonitorBindings(ctx, [spec]); return { onRequest: binding.onRequest, requestRules: binding.requestRules }; },
    async run(ctx) {
      const tui = await startMonitorTui(ctx); ctx.identity.arm(ctx.sessionIds.root!, "start"); await tui.prompt("Run M08 invalid regex and recovery.");
      await monitorDone(ctx, tui, "M08_RECOVERED_PARENT_IDLE"); await noMonitorFile(ctx, "m08.forbidden");
      expect(binding.results.get("M08-list-empty").tasks).toEqual([]); expect(await monitorFile(ctx, "m08.starts")).toBe("started\n");
      await monitorGate(ctx, "m08");
      await monitorDone(ctx, tui, "M08_RECOVERY_VERIFIED"); ctx.identity.finish(ctx.sessionIds.root!);
      await noMonitorFile(ctx, "m08.forbidden"); expect(binding.results.get("M08-read").events.map((event: any) => event.text)).toEqual(["M08_ACTUAL_EVENT"]);
      expect(binding.results.get("M08-read").status).toBe("completed");
      ctx.observe("monitor-recovery-oracle", { invalidStartSideEffects: false, retainedTasksAfterInvalidStart: 0, validStarts: 1 });
      await tui.save("monitor-invalid-regex-recovery");
    },
  });
});

it("M06 monitor_stop terminates the process tree and remains idempotent after terminal settlement", { timeout: 120_000 }, async () => {
  const spec = { id: "M06-start", alias: "m06", command: monitorCommand("m06", "tree") }; let binding: MonitorBindings;
  const settledGate = monitorCall("start", "reason:5");
  await runReplayScenario({ id: "M06", title: "monitor_stop 真正退出进程树，重复停止不改变终态或追加迟到事件", configVersion: "monitor-stop-2",
    configure: configureMonitor, portableTools: portableMonitorTools,
    upstream: async () => agentProvider((call, body): Reply => {
      if (call.execution === "start" && call.call === "reason:1") return tools(discoverMonitor("M06"));
      if (call.execution === "start" && call.call === "reason:2") return tools(monitorTool(spec.id, "monitor_start", { command: spec.command, label: "M06 process tree" }));
      if (call.execution === "start" && call.call === "reason:3") return text("M06_PARENT_IDLE");
      if (call.execution === "start" && call.call === "reason:4") return tools(monitorTool("M06-stop-1", "monitor_stop", { task_id: binding.raw("m06"), reason: "M06 requested stop" }));
      if (call.execution === "start" && call.call === "reason:5") {
        expect(jsonResult(body, "M06-stop-1").cleanup.processTerminated).toBe(true);
        return tools(monitorTool("M06-stop-2", "monitor_stop", { task_id: binding.raw("m06"), reason: "M06 repeated stop" }));
      }
      if (call.execution === "start" && call.call === "reason:6") {
        expect(bodyText(body)).toContain("MCP task on monitor finished with status Cancelled");
        return tools(monitorTool("M06-read", "monitor_read", { task_id: binding.raw("m06"), after_seq: 0 }));
      }
      if (call.execution === "start" && call.call === "reason:7") return tools(monitorTool("M06-list-live", "monitor_list", {}));
      if (call.execution === "start" && call.call === "reason:8") return tools(monitorTool("M06-read-empty", "monitor_read", { task_id: binding.raw("m06"), after_seq: 1 }));
      if (call.execution === "start" && call.call === "reason:9") return text("M06_STOP_VERIFIED");
      throw new Error(`unexpected M06 ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new MonitorBindings(ctx, [spec]); return { onRequest: binding.onRequest, requestRules: binding.requestRules, gates: [{ identity: settledGate, afterBytes: 0 }] }; },
    async run(ctx) {
      const tui = await startMonitorTui(ctx); ctx.identity.arm(ctx.sessionIds.root!, "start"); await tui.prompt("Run M06 process-tree monitor.");
      await monitorDone(ctx, tui, "M06_PARENT_IDLE");
      const pids = [Number(await monitorFile(ctx, "m06.pid")), Number(await monitorFile(ctx, "m06.child"))];
      await monitorFile(ctx, "m06.flushed"); // stdout flush precedes this marker; stop then drains the real pipe.
      await tui.command("tasks");
      await tui.waitFor(ctx.checked((screen: string) => screen.includes("monitor") && /\[(?:Running|running)\]/u.test(screen)), "monitor is present before stop");
      await tui.save("monitor-before-stop-panel"); await closeMonitorPanel(ctx, tui);
      await tui.prompt("Stop M06 monitor twice and verify its retained final event."); await ctx.run.waitReached(settledGate, { timeout: 30_000 });
      await assertExited(pids);
      const ledgerAtExit = await readFile(path.join(ctx.world.work, "m06.ledger"), "utf8");
      await tui.command("tasks");
      const terminalPanel = await tui.waitFor(ctx.checked((screen: string) => screen.includes("No active tasks") && screen.includes("0 bg")), "actual host terminal event removes the live task row");
      expect(terminalPanel).not.toMatch(/\[(?:Running|running)\]/u); await tui.save("monitor-stop-terminal-panel"); await closeMonitorPanel(ctx, tui);
      ctx.run.release(settledGate); await monitorDone(ctx, tui, "M06_STOP_VERIFIED"); ctx.identity.finish(ctx.sessionIds.root!);
      const first = binding.results.get("M06-stop-1"), second = binding.results.get("M06-stop-2");
      expect(first.status).toBe("cancelled"); expect(first.cleanup).toEqual({ requested: true, processTerminated: true }); expect(second).toEqual(first);
      const page = binding.results.get("M06-read"); expect(page.events.map((event: any) => event.text)).toEqual(["M06_TREE_READY"]);
      expect(page.status).toBe("cancelled"); expect(binding.results.get("M06-read-empty")).toMatchObject({ events: [], nextSeq: 1, status: "cancelled", dropped: 0 });
      expect(binding.results.get("M06-list-live").tasks).toEqual([]);
      await monitorGate(ctx, "m06", ".never"); // Producer PIDs have exited: releasing their former gate cannot write anything.
      expect(await readFile(path.join(ctx.world.work, "m06.ledger"), "utf8")).toBe(ledgerAtExit); await noMonitorFile(ctx, "m06.ended");
      ctx.observe("monitor-stop-oracle", { pids, processTreeExited: true, terminal: first.status, repeatedStopIdentical: true, retainedSeq: [1] });
      await tui.save("monitor-stop-idempotent");
    },
  });
});
