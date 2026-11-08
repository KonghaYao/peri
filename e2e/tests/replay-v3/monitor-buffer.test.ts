import { readFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { agentProvider, expectResult, text, tools, type Reply } from "../replay-v2/agents-fixture.js";
import {
  MonitorBindings, assertPage, bodyText, configureMonitor, discoverMonitor, jsonResult, monitorCommand, monitorDone,
  monitorFile, monitorGate, monitorTool, portableMonitorTools, requestBody, startMonitorTui,
} from "./monitor-fixture.js";

const cases = [
  { id: "M02", title: "monitor 真实 stdout 与 EOF 末行保留，终态读完后空页稳定", mode: "streams", reads: 2, limit: 20, bytes: 65_536 },
  { id: "M03", title: "monitor 终态继续分页，exclusive cursor 单调且事件无遗漏无重复", mode: "pages", reads: 4, limit: 3, bytes: 65_536 },
  { id: "M04", title: "monitor ring 溢出精确报告 7 个丢弃事件，并恢复全部剩余序号", mode: "overflow", reads: 3, limit: 2, bytes: 65_536 },
  { id: "M05", title: "monitor 超长 Unicode 行在 UTF-8 边界截断，小读预算仍推进完整事件", mode: "unicode", reads: 3, limit: 20, bytes: 1 },
  { id: "M12", title: "monitor 非零退出传播失败状态和 stderr，失败终态仍可读全部事件", mode: "failure", reads: 2, limit: 20, bytes: 65_536 },
] as const;

describe("real monitor capture and retained buffers", () => {
  for (const entry of cases) it(`${entry.id} ${entry.title}`, { timeout: 120_000 }, async () => {
    const alias = entry.id.toLowerCase(), spec = { id: `${entry.id}-start`, alias, command: monitorCommand(alias, entry.mode) };
    let binding: MonitorBindings;
    await runReplayScenario({
      id: entry.id, title: entry.title, configVersion: `monitor-buffer-${entry.id}-2`, configure: configureMonitor,
      portableTools: portableMonitorTools,
      upstream: async () => agentProvider((call, body): Reply => {
        if (call.execution === "start" && call.call === "reason:1") return tools(discoverMonitor(entry.id));
        if (call.execution === "start" && call.call === "reason:2") {
          expectResult(body, `${entry.id}-discover`, "mcp__monitor__monitor_start");
          return tools(monitorTool(spec.id, "monitor_start", { command: spec.command, label: `${entry.id} buffer`, ...(entry.mode === "overflow" ? { max_events: 3 } : {}) }));
        }
        if (call.execution === "start" && call.call === "reason:3") {
          expectResult(body, spec.id, "monitor_id: monitor-"); return text(`${entry.id}_PARENT_IDLE`);
        }
        if (call.execution === "start" && Number(call.call.split(":")[1]) >= 4) {
          const index = Number(call.call.split(":")[1]) - 3;
          if (index === 1) {
            expect(bodyText(body)).toContain("MCP task on monitor finished with status");
            expect(bodyText(body)).not.toContain(`${entry.id}_STDOUT`);
          }
          if (index <= entry.reads) {
            const after = index === 1 ? 0 : jsonResult(body, `${entry.id}-read-${index - 1}`).nextSeq;
            return tools(monitorTool(`${entry.id}-read-${index}`, "monitor_read", { task_id: binding.raw(alias), after_seq: after, limit: entry.limit, max_bytes: entry.bytes }));
          }
          expect(jsonResult(body, `${entry.id}-read-${entry.reads}`).events).toEqual([]);
          return text(`${entry.id}_BUFFER_VERIFIED`);
        }
        throw new Error(`unexpected ${entry.id} call ${JSON.stringify(call)}`);
      }),
      runOptions(ctx) { binding = new MonitorBindings(ctx, [spec]); return { onRequest: binding.onRequest, requestRules: binding.requestRules }; },
      async run(ctx) {
        const tui = await startMonitorTui(ctx);
        ctx.identity.arm(ctx.sessionIds.root!, "start"); await tui.prompt(`Run ${entry.id} real monitor buffer test.`);
        const before = await monitorDone(ctx, tui, `${entry.id}_PARENT_IDLE`);
        expect(before).toMatch(/monitor_start|ExecuteExtraTool/u);
        expect(await monitorFile(ctx, `${alias}.starts`)).toBe("started\n");
        // The owner is suspended waiting for its monitor. Its terminal reminder resumes this execution.
        await monitorGate(ctx, alias);
        const screen = await monitorDone(ctx, tui, `${entry.id}_BUFFER_VERIFIED`);
        expect(screen).toMatch(/monitor_read|ExecuteExtraTool/u);
        ctx.identity.finish(ctx.sessionIds.root!);
        const pages = Array.from({ length: entry.reads }, (_, i) => binding.results.get(`${entry.id}-read-${i + 1}`));
        let cursor = 0;
        for (const page of pages) { assertPage(page, cursor); expect(page.status).toBe(entry.mode === "failure" ? "failed" : "completed"); cursor = page.nextSeq; }
        const events = pages.flatMap((page) => page.events);
        expect(new Set(events.map((event: any) => event.seq)).size).toBe(events.length);
        const ledger = (await readFile(path.join(ctx.world.work, `${alias}.ledger`), "utf8")).trim().split("\n").map((line) => JSON.parse(line));
        if (entry.mode === "streams") {
          expect(events.map((event: any) => ({ text: event.text, stream: event.stream, partial: event.partial })).sort((a, b) => a.text.localeCompare(b.text)))
            .toEqual(ledger.map((event: any) => ({ text: event.text, stream: event.stream, partial: !event.newline })).sort((a, b) => a.text.localeCompare(b.text)));
          expect(events.map((event: any) => event.seq)).toEqual([1, 2]);
        } else if (entry.mode === "pages") {
          expect(ledger).toHaveLength(7); expect(pages.map((page) => page.nextSeq)).toEqual([3, 6, 7, 7]);
          expect(pages.map((page) => page.truncated)).toEqual([true, true, false, false]);
          expect(events.map((event: any) => event.text)).toEqual(ledger.map((event: any) => event.text));
          expect(events.map((event: any) => event.seq)).toEqual([1, 2, 3, 4, 5, 6, 7]);
        } else if (entry.mode === "overflow") {
          expect(ledger).toHaveLength(10); expect(pages.map((page) => page.dropped)).toEqual([7, 7, 7]);
          expect(events.map((event: any) => event.seq)).toEqual([8, 9, 10]);
          expect(events.map((event: any) => event.text)).toEqual(ledger.slice(7).map((event: any) => event.text));
          expect(events.map((event: any) => event.dropped)).toEqual([1, 1, 1]);
        } else if (entry.mode === "unicode") {
          expect(ledger).toHaveLength(2); expect(events).toHaveLength(2);
          const long = events[0]!; expect(long.bytes).toBe(35_000); expect(long.truncated).toBe(true); expect(long.partial).toBe(false);
          expect(Buffer.byteLength(long.text)).toBeLessThanOrEqual(16_384); expect(Buffer.byteLength(long.text)).toBeGreaterThanOrEqual(16_380);
          expect(ledger[0].text.startsWith(long.text)).toBe(true); expect(events[1]!.text).toBe("M05_AFTER_LONG");
          expect(pages.map((page) => page.nextSeq)).toEqual([1, 2, 2]); expect(pages.map((page) => page.truncated)).toEqual([true, true, false]);
        } else {
          expect(ledger).toEqual([{ text: "M12_FAILURE", stream: "stderr", newline: true }]);
          expect(events).toHaveLength(1); expect(events[0]).toMatchObject({ seq: 1, text: "M12_FAILURE", stream: "stderr", partial: false, truncated: false });
        }
        expect(pages.at(-1)!.events).toEqual([]);
        const terminal = requestBody(ctx, "start", "reason:4"), terminalText = bodyText(terminal);
        expect(terminalText).toContain(binding.raw(alias)); expect(terminalText).toContain(binding.host(alias));
        expect(terminalText.match(new RegExp(`MCP task on monitor finished with status ${entry.mode === "failure" ? "Failed" : "Completed"}\\.`, "gu"))).toHaveLength(1);
        if (entry.mode === "failure") expect(terminalText).toContain("Command exited with status 7");
        expect(ctx.requests.filter((request) => request.context.execution === "start" && request.context.call === "reason:4")).toHaveLength(1);
        ctx.observe("monitor-buffer-oracle", { raw: binding.raw(alias), host: binding.host(alias), sourceLines: ledger.length,
          eventSeq: events.map((event: any) => event.seq), cursors: pages.map((page) => page.nextSeq), dropped: pages[0].dropped, status: pages[0].status });
        await tui.save("monitor-buffer-verified");
      },
    });
  });
});
