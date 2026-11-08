import { writeFile } from "node:fs/promises";
import path from "node:path";
import { expect } from "vitest";
import type { ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import { REPLAY_BINARY, replayEnvironment } from "../../helpers/llm-replay-tui.js";
import { startStdioAcpFixture } from "../../helpers/stdio-acp-fixture.js";
import type { AgentIdentities } from "./async-agent-fixture.js";

/** Real JSON-RPC server, with the same temporary store later reopened by PTY. */
export async function startBackgroundAcp(ctx: ReplayScenarioContext, actors: AgentIdentities) {
  const notifications: any[] = [];
  const peer = await startStdioAcpFixture({ binary: REPLAY_BINARY, cwd: ctx.world.work, home: ctx.world.home,
    env: replayEnvironment(ctx.world, { proxy: ctx.proxy.url, run: ctx.runId }),
    onNotification: (message) => notifications.push(message), onServerRequest() {},
    handleServerRequest(request) { return { error: { code: -32601, message: `unexpected async-agent server request: ${request.method}` } }; },
  });
  let closed = false;
  const sessionId = ctx.sessionIds.root!;
  const healthy = () => {
    expect(ctx.proxy.snapshot(ctx.runId).errors).toEqual([]);
    expect(ctx.upstream?.errors ?? []).toEqual([]);
  };
  const events = () => notifications.filter((message) => message.method === "peri/agent_event" && message.params?.sessionId === sessionId)
    .map((message) => JSON.parse(message.params.event_json));
  const executions = () => [...new Map(events().filter((event) => event.type === "execution_started" || event.type === "user_input_run_started")
    .map((event) => [event.value.request_id, event.value])).values()];
  const activeExecutions = () => executions().filter((execution) => !notifications.some((message) => message.method === "peri/agent_event_done"
    && message.params?.sessionId === sessionId && message.params?.requestId === execution.request_id));
  const snapshot = async () => {
    const response = await peer.call("session/input/snapshot", { sessionId }); expect(response.error, JSON.stringify(response)).toBeUndefined(); return response.result;
  };
  const done = async (requestId: string, reason = "end_turn", timeout = 30_000) => {
    await expect.poll(() => { healthy(); return notifications.filter((message) => message.method === "peri/agent_event_done"
      && message.params?.sessionId === sessionId && message.params?.requestId === requestId).map((message) => message.params.stopReason); }, { timeout }).toEqual([reason]);
  };
  try {
    expect((await peer.call("initialize", { protocolVersion: 1, clientCapabilities: { _meta: { "peri.userInputQueue": true, "peri.agentEvent": true, "peri.agentEventDone": true } } })).error).toBeUndefined();
    const loaded = await peer.call("session/load", { sessionId, cwd: ctx.world.work, mcpServers: [] }); expect(loaded.error, JSON.stringify(loaded)).toBeUndefined();
  } catch (error) { await peer.close(); throw error; }
  return { peer, notifications, events, executions, activeExecutions, snapshot, done, healthy,
    async enqueue(action: string, content: string, inputId: string) {
      const before = await snapshot(), offset = notifications.length;
      expect(before.activeRequestId).toBeUndefined(); expect(before.items).toEqual([]); actors.arm(action);
      const response = await peer.call("session/input/enqueue", { sessionId, generation: before.generation,
        commandId: `async-agent-${inputId}`, inputId, content, originalDraft: content });
      expect(response.error, JSON.stringify(response)).toBeUndefined();
      let requestId: string | undefined;
      await expect.poll(() => { healthy();
        const starts = notifications.slice(offset).filter((message) => message.method === "peri/agent_event" && message.params?.sessionId === sessionId)
          .map((message) => JSON.parse(message.params.event_json)).filter((event) => event.type === "user_input_run_started" && event.value.generation === before.generation);
        expect(starts.length).toBeLessThanOrEqual(1); requestId = starts[0]?.value.request_id; return requestId;
      }, { timeout: 30_000 }).toBeDefined();
      return { requestId: requestId!, generation: before.generation, inputId };
    },
    async cancel(requestId?: string) {
      const active = activeExecutions();
      const execution = requestId ? active.find((entry) => entry.request_id === requestId) : active.at(-1);
      const target = execution?.request_id;
      expect(target, "an actual current execution must exist to cancel").toBeDefined();
      // Match AcpTuiClient::cancel: only mailbox-managed input runs use targeted cancellation.
      // Internal continuations are external attempts and use the current-session Stop contract.
      const managed = events().some((event) => event.type === "user_input_run_started" && event.value.request_id === target);
      peer.notify("session/cancel", managed ? { sessionId, generation: execution.generation, requestId: target } : { sessionId });
      await done(target, "cancelled");
    },
    async completedTask(marker: string) {
      // The host owns idle background notifications after the per-turn legacy forwarder has ended.
      await expect.poll(() => { healthy(); return notifications.filter((message) => message.method === "peri/unstable_event"
        && message.params?.sessionId === sessionId && message.params?.event === "bg-task-completed"
        && message.params?.data?.output_preview === marker).length; }, { timeout: 30_000 }).toBe(1);
    },
    async idle() { await expect.poll(() => { healthy(); return activeExecutions(); }, { timeout: 30_000 }).toEqual([]); },
    async close() {
      if (closed) return; closed = true;
      await writeFile(path.join(ctx.world.evidence, "async-agent-acp-notifications.json"), JSON.stringify(notifications, null, 2));
      await writeFile(path.join(ctx.world.evidence, "async-agent-acp-diagnostics.json"), await peer.diagnostics());
      try { expect((await peer.call("session/close", { sessionId })).error).toBeUndefined(); }
      finally {
        const exitCode = await peer.close();
        await writeFile(path.join(ctx.world.evidence, "async-agent-acp-process.json"), JSON.stringify({ exitCode, graceful: exitCode === 0 }, null, 2));
        expect(exitCode, "real ACP process must shut down gracefully").toBe(0);
      }
    },
  };
}
