import { writeFile } from "node:fs/promises";
import path from "node:path";
import { expect, it } from "vitest";
import { runReplayScenario, type ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import { agentProvider, expectResult, text, tool, tools, type Reply } from "../replay-v2/agents-fixture.js";
import { assertExited } from "./async-shell-fixture.js";
import { configureMonitor, monitorCommand, monitorDone, monitorFile, monitorGate, startMonitorTui } from "./monitor-fixture.js";
import { MonitorWire } from "./monitor-wire-fixture.js";

async function readActualReport(ctx: ReplayScenarioContext, id: string, content: string) {
  const file = `${id.toLowerCase()}-wire-result.txt`;
  await writeFile(path.join(ctx.world.work, file), content);
  const tui = await startMonitorTui(ctx); ctx.identity.arm(ctx.sessionIds.root!, "report");
  await tui.prompt(`Read the actual ${id} MCP lifecycle report.`);
  const screen = await monitorDone(ctx, tui, `${id}_WIRE_VERIFIED`); expect(screen).toContain("Read");
  ctx.identity.finish(ctx.sessionIds.root!); await tui.save("monitor-wire-report-visible");
}
const reportProvider = (id: string) => agentProvider((call, body): Reply => {
  if (call.execution === "report" && call.call === "reason:1") return tools(tool(`${id}-report`, "Read", { file_path: `${id.toLowerCase()}-wire-result.txt` }));
  if (call.execution === "report" && call.call === "reason:2") { expectResult(body, `${id}-report`, `${id}_REAL_MCP_PASS`); return text(`${id}_WIRE_VERIFIED`); }
  throw new Error(`unexpected ${id} ${JSON.stringify(call)}`);
});

it("M10 one real monitor MCP instance enforces owner scope on tools and Tasks methods", { timeout: 120_000 }, async () => {
  await runReplayScenario({ id: "M10", title: "同一真实 monitor MCP 实例跨 owner 的 read/stop/list/get/cancel 隔离", configVersion: "monitor-wire-owner-2",
    sessionNames: ["root", "other"], configure: configureMonitor, upstream: () => reportProvider("M10"),
    runOptions(ctx) { return { onRequest({ context, request }: any) { if (ctx.mode === "record") ctx.upstream!.correlate(context, request.body); } }; },
    async run(ctx) {
      const server = new MonitorWire(ctx, "M10"); const pids: number[] = [];
      try {
        await server.open(); const ownerA = ctx.sessionIds.root!, ownerB = ctx.sessionIds.other!;
        const taskA = await server.start(monitorCommand("m10a", "tree"), ownerA, "M10 owner A");
        const taskB = await server.start(monitorCommand("m10b", "tree"), ownerB, "M10 owner B"); expect(taskA).not.toBe(taskB);
        for (const alias of ["m10a", "m10b"]) {
          pids.push(Number(await monitorFile(ctx, `${alias}.pid`)), Number(await monitorFile(ctx, `${alias}.child`))); await monitorFile(ctx, `${alias}.flushed`);
        }
        const listA = server.result(await server.tool("monitor_list", {}, ownerA)).tasks;
        const listB = server.result(await server.tool("monitor_list", {}, ownerB)).tasks;
        expect(listA.map((task: any) => task.taskId)).toEqual([taskA]); expect(listB.map((task: any) => task.taskId)).toEqual([taskB]);
        // This is real server-side filtering: neither the wire client nor the assertion filters a result.
        const readDenied = await server.tool("monitor_read", { task_id: taskA }, ownerB);
        const readUnknown = await server.tool("monitor_read", { task_id: "monitor-unknown" }, ownerB);
        expect(readDenied.result.isError).toBe(true); expect(readDenied.result.content).toEqual(readUnknown.result.content);
        const stopDenied = await server.tool("monitor_stop", { task_id: taskA }, ownerB);
        expect(stopDenied.result.isError).toBe(true); expect(JSON.stringify(stopDenied)).toContain("unknown task");
        const getDenied = await server.call("tasks/get", { taskId: taskA }, ownerB);
        const getUnknown = await server.call("tasks/get", { taskId: "monitor-unknown" }, ownerB);
        expect(getDenied.error).toEqual(getUnknown.error); expect(getDenied.error.message).toBe("unknown task");
        const cancelDenied = await server.call("tasks/cancel", { taskId: taskA }, ownerB); expect(cancelDenied.error).toEqual(getUnknown.error);
        const aliveA = await server.call("tasks/get", { taskId: taskA }, ownerA); expect(aliveA.result.status).toBe("working");
        const generic = server.result(await server.tool("monitor_list", {}, undefined)).tasks;
        expect(generic.map((task: any) => task.taskId).sort()).toEqual([taskA, taskB].sort());
        // Owner is a host scope, not an authentication claim. Generic MCP mode intentionally sees all.
        const stoppedB = server.result(await server.tool("monitor_stop", { task_id: taskB }, ownerB)); expect(stoppedB.status).toBe("cancelled");
        expect((await server.call("tasks/get", { taskId: taskA }, ownerA)).result.status).toBe("working");
        const stoppedA = server.result(await server.tool("monitor_stop", { task_id: taskA }, ownerA)); expect(stoppedA.cleanup.processTerminated).toBe(true);
        await assertExited(pids);
        const retainedA = server.result(await server.tool("monitor_read", { task_id: taskA }, ownerA));
        const retainedB = server.result(await server.tool("monitor_read", { task_id: taskB }, ownerB));
        expect(retainedA.events.map((event: any) => event.text)).toEqual(["M10A_TREE_READY"]);
        expect(retainedB.events.map((event: any) => event.text)).toEqual(["M10B_TREE_READY"]);
        expect(server.result(await server.tool("monitor_list", {}, ownerA)).tasks).toEqual([]);
        expect(server.result(await server.tool("monitor_list", { include_ended: true }, ownerA)).tasks.map((task: any) => task.taskId)).toEqual([taskA]);
        ctx.observe("monitor-owner-wire-oracle", { ownerA, ownerB, taskA, taskB, pids, denied: ["read", "stop", "get", "cancel"], genericMode: "all", processesExited: true });
      } finally { await server.close(); }
      await readActualReport(ctx, "M10", "M10_REAL_MCP_PASS\nowner A and B lists isolated; cross-owner read stop get cancel denied; generic sees both; process trees exited\n");
    },
  });
});

it("M11 monitor instance restart stops processes and never revives old in-memory task IDs", { timeout: 120_000 }, async () => {
  await runReplayScenario({ id: "M11", title: "monitor 实例关闭清理进程树，重启后的旧 task ID 不复活且新任务正常", configVersion: "monitor-wire-restart-2",
    configure: configureMonitor, upstream: () => reportProvider("M11"),
    runOptions(ctx) { return { onRequest({ context, request }: any) { if (ctx.mode === "record") ctx.upstream!.correlate(context, request.body); } }; },
    async run(ctx) {
      const first = new MonitorWire(ctx, "M11-before"); let oldTask: string; const pids: number[] = [];
      try {
        await first.open(); oldTask = await first.start(monitorCommand("m11old", "tree"), ctx.sessionIds.root!, "M11 old instance");
        pids.push(Number(await monitorFile(ctx, "m11old.pid")), Number(await monitorFile(ctx, "m11old.child")));
        await monitorFile(ctx, "m11old.flushed"); expect((await first.call("tasks/get", { taskId: oldTask }, ctx.sessionIds.root!)).result.status).toBe("working");
      } finally { await first.close(); }
      // Transport EOF and process exit are observable barriers; no fixed sleep proves cleanup.
      await assertExited(pids);
      const second = new MonitorWire(ctx, "M11-after");
      try {
        await second.open(); const owner = ctx.sessionIds.root!;
        expect(second.result(await second.tool("monitor_list", { include_ended: true }, owner)).tasks).toEqual([]);
        const stale = await second.tool("monitor_read", { task_id: oldTask! }, owner); expect(stale.result.isError).toBe(true); expect(JSON.stringify(stale)).toContain("unknown task");
        const fresh = await second.start(monitorCommand("m11new", "plain"), owner, "M11 new instance"); expect(fresh).not.toBe(oldTask!);
        await monitorFile(ctx, "m11new.pid"); await monitorGate(ctx, "m11new");
        await expect.poll(async () => (await second.call("tasks/get", { taskId: fresh }, owner)).result.status, { timeout: 15_000, interval: 30 }).toBe("completed");
        const page = second.result(await second.tool("monitor_read", { task_id: fresh }, owner)); expect(page.events.map((event: any) => event.text)).toEqual(["M11NEW_ACTUAL_EVENT"]);
        expect(page.nextSeq).toBe(1); expect(page.status).toBe("completed");
        expect(await monitorFile(ctx, "m11old.starts")).toBe("started\n"); expect(await monitorFile(ctx, "m11new.starts")).toBe("started\n");
        ctx.observe("monitor-restart-wire-oracle", { oldTask: oldTask!, freshTask: fresh, oldPids: pids, oldProcessesExited: true, staleIdRejected: true, newEventSeq: [1] });
      } finally { await second.close(); }
      await readActualReport(ctx, "M11", "M11_REAL_MCP_PASS\ntransport EOF stopped old process tree; new instance rejected stale task ID; new task captured exactly one event\n");
    },
  });
});
