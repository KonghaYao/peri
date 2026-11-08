import { createServer, type ServerResponse } from "node:http";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario, type ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import { ScenarioIdentity } from "../../helpers/llm-replay-identity.js";
import { declareReplayAmbientBindings } from "../../helpers/llm-replay-runtime.js";
import {
  createReplayWorld, prepareReplaySessions, REPLAY_BINARY, replayEnvironment, ReplayTui,
  syntheticRequestEvidence, type ReplayWorld,
} from "../../helpers/llm-replay-tui.js";
import { startStdioAcpFixture } from "../../helpers/stdio-acp-fixture.js";

const bindingModule = new URL("../../llm-replay/request-bindings.mjs", import.meta.url).href;
const identity = (owner = "root", call = "reason:1", attempt = 1) => ({ owner, execution: "turn-1", call, attempt, purpose: "reason" });

function answer(text: string, tool?: { id: string; name: string; input: object }): string {
  const event = (type: string, data: object) => `event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`;
  const start = event("message_start", { message: { id: "isolation-fixture", type: "message", role: "assistant", model: "fixture-model",
    content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 0 } } });
  const content = tool
    ? event("content_block_start", { index: 0, content_block: { type: "tool_use", id: tool.id, name: tool.name, input: {} } })
      + event("content_block_delta", { index: 0, delta: { type: "input_json_delta", partial_json: JSON.stringify(tool.input) } })
    : event("content_block_start", { index: 0, content_block: { type: "text", text: "" } })
      + event("content_block_delta", { index: 0, delta: { type: "text_delta", text } });
  return start + content + event("content_block_stop", { index: 0 })
    + event("message_delta", { delta: { stop_reason: tool ? "tool_use" : "end_turn", stop_sequence: null }, usage: { output_tokens: 8 } })
    + event("message_stop", {});
}

async function fixture(handler: (body: any, response: ServerResponse, index: number) => void) {
  const errors: string[] = [];
  let count = 0;
  const server = createServer((request, response) => { void (async () => {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    if (!Array.isArray(body.tools) || !body.tools.length) throw new Error("undeclared auxiliary call");
    if (request.headers["x-peri-replay-context"] || request.headers["x-peri-replay-target"]) throw new Error("private routing metadata leaked upstream");
    handler(body, response, ++count);
  })().catch((error) => { errors.push(String(error)); response.writeHead(500); response.end(String(error)); }); });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("fixture did not bind");
  return { url: `http://127.0.0.1:${address.port}`, errors,
    close: () => new Promise<void>((resolve) => { server.closeAllConnections(); server.close(() => resolve()); }) };
}

async function startSessions(ctx: ReplayScenarioContext) {
  const notifications: any[] = [];
  const peer = await startStdioAcpFixture({ binary: REPLAY_BINARY, cwd: ctx.world.work, home: ctx.world.home,
    env: replayEnvironment(ctx.world, { proxy: ctx.proxy.url, run: ctx.runId }),
    onNotification: (message) => notifications.push(message), onServerRequest() {},
    handleServerRequest(request) { return { error: { code: -32601, message: `undeclared server request ${request.method}` } }; },
  });
  const healthy = () => expect(ctx.proxy.snapshot(ctx.runId).errors).toEqual([]);
  try {
    const initialized = await peer.call("initialize", { protocolVersion: 1, clientCapabilities: { _meta: { "peri.userInputQueue": true } } });
    expect(initialized.error).toBeUndefined();
    for (const sessionId of Object.values(ctx.sessionIds)) {
      const loaded = await peer.call("session/load", { sessionId, cwd: ctx.world.work, mcpServers: [] });
      expect(loaded.error, JSON.stringify(loaded)).toBeUndefined();
    }
  } catch (error) { await peer.close(); throw error; }
  const attempts = new Map<string, { generation: string; inputId: string; offset: number }>();
  return {
    peer, notifications,
    text(alias: string) { return JSON.stringify(notifications.filter((message) => message.params?.sessionId === ctx.sessionIds[alias])); },
    async enqueue(alias: string) {
      const sessionId = ctx.sessionIds[alias]!;
      const before = await peer.call("session/input/snapshot", { sessionId });
      expect(before.error).toBeUndefined();
      expect(before.result.activeRequestId).toBeUndefined();
      expect(before.result.items).toEqual([]);
      const inputId = alias === "A" ? "00000000-0000-0000-0000-000000000101" : "00000000-0000-0000-0000-000000000102";
      attempts.set(alias, { generation: before.result.generation, inputId, offset: notifications.length });
      ctx.identity.arm(sessionId, "turn-1");
      const receipt = await peer.call("session/input/enqueue", { sessionId, generation: before.result.generation,
        commandId: `I01-${alias}`, inputId, content: "I01_IDENTICAL_USER_PROMPT", originalDraft: "I01_IDENTICAL_USER_PROMPT" });
      expect(receipt.error, JSON.stringify(receipt)).toBeUndefined();
    },
    async done(alias: string) {
      const sessionId = ctx.sessionIds[alias]!;
      const attempt = attempts.get(alias)!;
      let requestId: string | undefined;
      await expect.poll(() => {
        healthy();
        const starts = notifications.slice(attempt.offset).filter((message) => message.method === "peri/agent_event" && message.params?.sessionId === sessionId)
          .map((message) => JSON.parse(message.params.event_json)).filter((event) => event.type === "user_input_run_started" && event.value.generation === attempt.generation);
        expect(starts.length).toBeLessThanOrEqual(1);
        requestId = starts[0]?.value.request_id;
        return notifications.filter((message) => message.method === "peri/agent_event_done" && message.params?.sessionId === sessionId &&
          requestId !== undefined && message.params?.requestId === requestId).map((message) => message.params.stopReason);
      }, { timeout: 30_000 }).toEqual(["end_turn"]);
      await expect.poll(async () => {
        healthy();
        const state = await peer.call("session/input/snapshot", { sessionId });
        expect(state.error).toBeUndefined();
        return { active: state.result.activeRequestId ?? null, items: state.result.items, terminal: state.result.terminalItems };
      }, { timeout: 30_000 }).toMatchObject({ active: null, items: [], terminal: [{ inputId: attempt.inputId, state: "delivered" }] });
      ctx.identity.finish(sessionId);
    },
    async close() {
      for (const sessionId of Object.values(ctx.sessionIds)) expect((await peer.call("session/close", { sessionId })).error).toBeUndefined();
      expect(await peer.close()).toBe(0);
    },
  };
}

describe("I01-I02 real session and replay-run isolation", () => {
  it("I01 identical prompts retain their session across reversed request and response order", async () => {
    await runReplayScenario({ id: "I01", title: "双会话相同输入反序请求与响应，分别恢复真实 TUI 历史", sessionNames: ["A", "B"],
      upstream: async () => fixture((body, response, index) => {
        if (!JSON.stringify(body.messages).includes("I01_IDENTICAL_USER_PROMPT") || index > 2) throw new Error("unexpected I01 request");
        response.writeHead(200, { "content-type": "text/event-stream" });
        response.end(answer(index === 1 ? "I01_SESSION_A_ONLY" : "I01_SESSION_B_ONLY"));
      }),
      runOptions: () => ({ gates: ["A", "B"].map((owner) => ({ identity: identity(owner), afterBytes: 0 })) }),
      run: async (ctx) => {
        const sessions = await startSessions(ctx);
        let closed = false;
        try {
          const requestOrder = ctx.mode === "record" ? ["A", "B"] : ["B", "A"];
          const responseOrder = [...requestOrder].reverse();
          for (const alias of requestOrder) {
            await sessions.enqueue(alias);
            await ctx.run.waitReached(identity(alias), { timeout: 30_000 });
          }
          expect(ctx.proxy.snapshot(ctx.runId).calls.filter((call: any) => ["reserved", "streaming"].includes(call.state))).toHaveLength(2);
          for (const alias of responseOrder) {
            ctx.run.release(identity(alias));
            await sessions.done(alias);
          }
          for (const [alias, other] of [["A", "B"], ["B", "A"]]) {
            expect(sessions.text(alias!)).toContain(`I01_SESSION_${alias}_ONLY`);
            expect(sessions.text(alias!)).not.toContain(`I01_SESSION_${other}_ONLY`);
          }
          await sessions.close(); closed = true;
          ctx.observe("request_and_response_order", { requestOrder, responseOrder, notifications: sessions.notifications });
          for (const [alias, other] of [["A", "B"], ["B", "A"]]) {
            const tui = await ctx.startTui(alias!);
            const screen = await tui.waitFor(ctx.checked((value: string) => value.includes(`I01_SESSION_${alias}_ONLY`)), "correct session history visible");
            expect(screen).not.toContain(`I01_SESSION_${other}_ONLY`);
            await tui.save("isolated-history");
            ctx.observe(`${alias}_visible_history`, screen);
            await tui.stop();
          }
          const observed = ctx.proxy.snapshot(ctx.runId).calls.map((call: any) => call.identity.owner);
          expect(observed).toEqual(requestOrder);
        } finally {
          await writeFile(path.join(ctx.world.evidence, "I01-notifications.json"), JSON.stringify(sessions.notifications, null, 2));
          if (!closed) await sessions.peer.close();
        }
      },
    });
  });

  it("I02 one sealed cassette serves concurrent fresh worlds with isolated retry, files, and TUI state", async () => {
    const retryGate = identity("root", "reason:1", 2);
    await runReplayScenario({ id: "I02", title: "同份录制双 run 并发重放，真实重试和文件状态互不串扰",
      upstream: async (world) => fixture((body, response, index) => {
        if (!JSON.stringify(body.messages).includes("I02_IDENTICAL_RETRY_WRITE")) throw new Error("unexpected I02 input");
        if (index === 1) {
          response.writeHead(429, { "content-type": "application/json", "retry-after": "0" });
          response.end(JSON.stringify({ type: "error", error: { type: "rate_limit_error", message: "I02 controlled retry" } }));
        } else if (index === 2) {
          response.writeHead(200, { "content-type": "text/event-stream" });
          response.end(answer("", { id: "I02-write", name: "Write", input: { file_path: path.join(world.work, "isolation.txt"), content: "I02_FILE_CONTENT\n" } }));
        } else if (index === 3) {
          const result = (body.messages ?? []).flatMap((message: any) => Array.isArray(message.content) ? message.content : [])
            .find((block: any) => block.type === "tool_result" && block.tool_use_id === "I02-write");
          if (!result || result.is_error) throw new Error("real Write did not succeed");
          response.writeHead(200, { "content-type": "text/event-stream" });
          response.end(answer("I02_RETRY_AND_WRITE_COMPLETED"));
        } else throw new Error("extra I02 call");
      }),
      runOptions: () => ({ gates: [{ identity: retryGate, afterBytes: 0 }] }),
      run: async (ctx) => {
        const first = await ctx.startTui("root");
        let second: ReplayTui | undefined;
        let secondWorld: ReplayWorld | undefined;
        let secondRun: any;
        let secondIdentity: ScenarioIdentity | undefined;
        let secondSession: string | undefined;
        let secondRunId: string | undefined;
        try {
          ctx.identity.arm(ctx.sessionIds.root!, "turn-1");
          await first.prompt("I02_IDENTICAL_RETRY_WRITE");
          await ctx.run.waitReached(retryGate, { timeout: 30_000 });
          if (ctx.mode === "replay") {
            const { createRequestBindingAdapter } = await import(bindingModule);
            secondWorld = await createReplayWorld();
            secondWorld.evidence = path.join(ctx.world.evidence, "independent-run");
            await mkdir(secondWorld.evidence, { recursive: true });
            [secondSession] = await prepareReplaySessions(secondWorld);
            secondRunId = `${ctx.runId}-independent`;
            secondIdentity = new ScenarioIdentity([{ live: secondSession!, canonical: "root" }]);
            const bindings = createRequestBindingAdapter({ mode: "replay", runId: secondRunId, declarations: ctx.bindings.snapshotDeclarations() });
            await declareReplayAmbientBindings(bindings, secondWorld);
            bindings.declareAndBind({ kind: "session", alias: "root", live: secondSession!, anchor: "ACP session/new result" });
            secondRun = ctx.proxy.createRun(secondRunId, { resolveContext: secondIdentity.resolve, bindings: bindings.registry,
              bindingOwnerFor: bindings.bindingOwnerFor, portableResponses: ctx.run.options.portableResponses,
              gates: [{ identity: retryGate, afterBytes: 0 }], onRequest: syntheticRequestEvidence(secondWorld, "replay") });
            second = await ReplayTui.start(secondWorld, { label: "I02-independent", sessionId: secondSession!, proxy: ctx.proxy.url, run: secondRunId });
            secondIdentity.arm(secondSession!, "turn-1");
            await second.prompt("I02_IDENTICAL_RETRY_WRITE");
            await secondRun.waitReached(retryGate, { timeout: 30_000 });
            expect(ctx.proxy.snapshot(ctx.runId).active).toBe(1);
            expect(ctx.proxy.snapshot(secondRunId).active).toBe(1);
            secondRun.release(retryGate);
            const screen = await second.waitFor((value) => {
              expect(ctx.proxy.snapshot(secondRunId).errors).toEqual([]);
              return value.includes("I02_RETRY_AND_WRITE_COMPLETED");
            }, "independent run finishes its own retry and Write", 45_000);
            await ctx.proxy.waitFor(secondRunId, (state: any) => state.active === 0 && state.calls.length === 3);
            expect(await readFile(path.join(secondWorld.work, "isolation.txt"), "utf8")).toBe("I02_FILE_CONTENT\n");
            await expect(readFile(path.join(ctx.world.work, "isolation.txt"), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
            expect(ctx.proxy.snapshot(ctx.runId).calls).toHaveLength(2);
            expect(ctx.proxy.snapshot(ctx.runId).calls[1].writtenBytes).toBe(0);
            await writeFile(path.join(secondWorld.work, "isolation.txt"), "I02_SECOND_WORLD_PRIVATE\n");
            ctx.observe("second_completed_while_first_held", { world: secondWorld, session: secondSession, screen, second: ctx.proxy.snapshot(secondRunId), first: ctx.proxy.snapshot(ctx.runId) });
          }
          ctx.run.release(retryGate);
          const firstScreen = await first.waitFor(ctx.checked((value: string) => value.includes("I02_RETRY_AND_WRITE_COMPLETED")), "first run completes its independent retry and Write", 45_000);
          await ctx.waitForCalls((state) => state.active === 0 && state.calls.length === 3);
          expect(await readFile(path.join(ctx.world.work, "isolation.txt"), "utf8")).toBe("I02_FILE_CONTENT\n");
          ctx.identity.finish(ctx.sessionIds.root!);
          const assertAttempts = (summary: any) => expect(summary.calls.map((call: any) => [call.identity.call, call.identity.attempt])).toEqual([
            ["reason:1", 1], ["reason:1", 2], ["reason:2", 1],
          ]);
          assertAttempts(ctx.proxy.snapshot(ctx.runId));
          ctx.observe("first_visible_completion", firstScreen);
          await first.save("retry-file-isolated");
          if (second && secondWorld && secondRunId && secondIdentity && secondSession) {
            secondIdentity.finish(secondSession);
            expect(await readFile(path.join(secondWorld.work, "isolation.txt"), "utf8")).toBe("I02_SECOND_WORLD_PRIVATE\n");
            assertAttempts(ctx.proxy.snapshot(secondRunId));
            const complete = ctx.proxy.completeRun(secondRunId);
            expect(complete.upstreamCalls).toBe(0);
            expect(secondWorld.work).not.toBe(ctx.world.work);
            expect(secondSession).not.toBe(ctx.sessionIds.root);
            await second.save("retry-file-isolated");
            await writeFile(path.join(secondWorld.evidence, "proxy-final.json"), JSON.stringify(complete, null, 2));
            await writeFile(path.join(secondWorld.evidence, "isolation-final.txt"), await readFile(path.join(secondWorld.work, "isolation.txt")));
            ctx.observe("independent_run_final", { world: secondWorld, run: complete });
          }
        } finally {
          await second?.save("final").catch(() => {});
          try { await second?.stop(); }
          finally {
            try { await first.stop(); }
            finally { if (secondWorld) await rm(secondWorld.root, { recursive: true, force: true }); }
          }
        }
      },
    });
  });
});
