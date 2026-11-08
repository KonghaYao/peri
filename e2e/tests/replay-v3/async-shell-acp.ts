import { writeFile } from "node:fs/promises";
import path from "node:path";
import { expect } from "vitest";
import type { ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import { REPLAY_BINARY, replayEnvironment } from "../../helpers/llm-replay-tui.js";
import { replayStorePath } from "../../helpers/replay-isolation.js";
import { startStdioAcpFixture } from "../../helpers/stdio-acp-fixture.js";

/** Queue-backed ACP sessions; every turn is correlated with its actual input and completion. */
export async function shellAcp(ctx: ReplayScenarioContext) {
  const notifications: any[] = [];
  const peer = await startStdioAcpFixture({ binary: REPLAY_BINARY, cwd: ctx.world.work, home: ctx.world.home,
    dbPath: replayStorePath(ctx.world), env: replayEnvironment(ctx.world, { proxy: ctx.proxy.url, run: ctx.runId }),
    onNotification(message) { notifications.push(message); }, onServerRequest() {},
    handleServerRequest(request) { return { error: { code: -32601, message: `undeclared shell fixture server request ${request.method}` } }; },
  });
  let nextInput = 1;
  const attempts = new Map<string, { generation: string; inputId: string; offset: number }>();
  const healthy = () => expect(ctx.proxy.snapshot(ctx.runId).errors).toEqual([]);
  const text = (alias: string) => JSON.stringify(notifications.filter(message => message.params?.sessionId === ctx.sessionIds[alias]));
  const events = (alias: string) => notifications.filter(message => message.method === "peri/agent_event" && message.params?.sessionId === ctx.sessionIds[alias])
    .map(message => JSON.parse(message.params.event_json));
  try {
    expect((await peer.call("initialize", { protocolVersion: 1, clientCapabilities: { _meta: {
      "peri.userInputQueue": true, "peri.agentEvent": true, "peri.agentEventDone": true, "peri.systemReminder": true,
    } } })).error).toBeUndefined();
    for (const sessionId of Object.values(ctx.sessionIds)) {
      expect((await peer.call("session/load", { sessionId, cwd: ctx.world.work, mcpServers: [] })).error).toBeUndefined();
    }
  } catch (error) {
    const exitCode = await peer.close();
    ctx.observe("shell-acp-process-exit", { exitCode, graceful: exitCode === 0 });
    throw error;
  }
  return {
    peer, notifications, text, events,
    async enqueue(alias: string, execution: string, content: string, continued = false) {
      const sessionId = ctx.sessionIds[alias]!;
      const state = await peer.call("session/input/snapshot", { sessionId });
      expect(state.error).toBeUndefined();
      if (!continued) expect(state.result.activeRequestId).toBeUndefined();
      else expect(state.result.activeRequestId).toBeTypeOf("string");
      expect(state.result.items).toEqual([]);
      const inputId = `00000000-0000-0000-0000-${String(nextInput++).padStart(12, "0")}`;
      if (!continued) {
        attempts.set(alias, { generation: state.result.generation, inputId, offset: notifications.length });
        ctx.identity.arm(sessionId, execution);
      }
      const receipt = await peer.call("session/input/enqueue", { sessionId, generation: state.result.generation,
        commandId: `${ctx.runId}-${alias}-${execution}-${inputId}`, inputId, content, originalDraft: content });
      expect(receipt.error, JSON.stringify(receipt)).toBeUndefined();
    },
    async suspended(alias: string, marker: string) {
      const attempt = attempts.get(alias)!;
      await expect.poll(() => {
        healthy();
        const tail = notifications.slice(attempt.offset).filter(message => message.method === "peri/agent_event" && message.params?.sessionId === ctx.sessionIds[alias])
          .map(message => JSON.parse(message.params.event_json));
        return { marker: text(alias).includes(marker), suspended: tail.some(event => event.type === "turn_suspended") };
      }, { timeout: 30_000 }).toEqual({ marker: true, suspended: true });
    },
    async done(alias: string, marker: string) {
      const attempt = attempts.get(alias)!;
      const sessionId = ctx.sessionIds[alias]!;
      await expect.poll(() => {
        healthy();
        const starts = notifications.slice(attempt.offset).filter(message => message.method === "peri/agent_event" && message.params?.sessionId === sessionId)
          .map(message => JSON.parse(message.params.event_json)).filter(event => event.type === "user_input_run_started" && event.value.generation === attempt.generation);
        expect(starts.length).toBeLessThanOrEqual(1);
        const requestId = starts[0]?.value.request_id;
        return notifications.filter(message => message.method === "peri/agent_event_done" && message.params?.sessionId === sessionId && requestId && message.params.requestId === requestId).map(message => message.params.stopReason);
      }, { timeout: 30_000 }).toEqual(["end_turn"]);
      await this.settled(alias, marker);
      ctx.identity.finish(sessionId);
    },
    async settled(alias: string, marker: string) {
      await expect.poll(async () => {
        healthy();
        const state = await peer.call("session/input/snapshot", { sessionId: ctx.sessionIds[alias] });
        expect(state.error).toBeUndefined();
        return { marker: text(alias).includes(marker), active: state.result.activeRequestId ?? null, pending: state.result.items };
      }, { timeout: 40_000 }).toEqual({ marker: true, active: null, pending: [] });
    },
    async tasks(alias: string) {
      const response = await peer.call("session/bg-tasks", { sessionId: ctx.sessionIds[alias] });
      expect(response.error).toBeUndefined();
      expect(response.result.source).toBe("live");
      return response.result;
    },
    async save(name: string) { await writeFile(path.join(ctx.world.evidence, `${name}-acp.json`), JSON.stringify({ notifications, diagnostics: await peer.diagnostics() }, null, 2)); },
    async closeTransport() {
      const exitCode = await peer.close();
      ctx.observe("shell-acp-process-exit", { exitCode, graceful: exitCode === 0 });
      return exitCode;
    },
    async close() {
      for (const sessionId of Object.values(ctx.sessionIds)) expect((await peer.call("session/close", { sessionId })).error).toBeUndefined();
      expect(await this.closeTransport()).toBe(0);
    },
  };
}
