import { createServer } from "node:http";
import { writeFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { REPLAY_BINARY, replayEnvironment } from "../../helpers/llm-replay-tui.js";
import { startStdioAcpFixture } from "../../helpers/stdio-acp-fixture.js";

const PREFIX_A = "I03_A_VISIBLE_BEFORE_CANCEL";
const FORBIDDEN_A = "I03_A_FORBIDDEN_LATE_SUFFIX";
const PREFIX_B = "I03_B_STILL_RUNNING";
const DONE_B = "I03_B_COMPLETED_INDEPENDENTLY";
const NEXT_A = "I03_A_NEW_EXECUTION_COMPLETED";
const identity = (owner: string, execution = "turn-1") => ({ owner, execution, call: "reason:1", attempt: 1, purpose: "reason" });

function event(type: string, data: object): string {
  return `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
}

function prefix(text: string): string {
  return event("message_start", { type: "message_start", message: {
    id: "I03-fixture", type: "message", role: "assistant", model: "fixture-model", content: [],
    stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 0 },
  } })
    + event("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } })
    + event("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } });
}

function suffix(text = ""): string {
  return (text ? event("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } }) : "")
    + event("content_block_stop", { type: "content_block_stop", index: 0 })
    + event("message_delta", { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: 8 } })
    + event("message_stop", { type: "message_stop" });
}

/** The source is used only by the explicit record command; replay loads its sealed cassette. */
async function recordingSource() {
  const server = createServer((request, response) => { void (async () => {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    const messages = JSON.stringify(body.messages);
    if (!Array.isArray(body.tools) || !body.tools.length) throw new Error("I03 does not declare auxiliary model calls");
    let result: string;
    if (messages.includes("I03_START_NEW_A")) result = prefix(NEXT_A) + suffix();
    else if (messages.includes("I03_START_CANCEL_A")) result = prefix(PREFIX_A) + suffix(FORBIDDEN_A);
    else if (messages.includes("I03_START_KEEP_B")) result = prefix(PREFIX_B) + suffix(DONE_B);
    else throw new Error("unexpected I03 model request");
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.end(result);
  })().catch((error) => { response.writeHead(500); response.end(String(error)); }); });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("I03 fixture source did not bind");
  return {
    url: `http://127.0.0.1:${address.port}`,
    close: () => new Promise<void>((resolve) => { server.closeAllConnections(); server.close(() => resolve()); }),
  };
}

describe("I03 — one cancelled session does not cancel its peer or its next execution", () => {
  it("同后端双会话：A取消、B继续，旧取消与未交付后缀不污染新执行或TUI历史", { timeout: 180_000 }, async () => {
    await runReplayScenario({
      id: "I03",
      title: "同后端双会话取消隔离与旧执行屏障",
      sessionNames: ["A", "B"],
      upstream: recordingSource,
      runOptions: () => ({ gates: [
        { identity: identity("A"), afterBytes: Buffer.byteLength(prefix(PREFIX_A)) },
        { identity: identity("B"), afterBytes: Buffer.byteLength(prefix(PREFIX_B)) },
        { identity: identity("A", "turn-2"), afterBytes: 0 },
      ] }),
      run: async (ctx) => {
        const notifications: any[] = [];
        const peer = await startStdioAcpFixture({
          binary: REPLAY_BINARY, cwd: ctx.world.work, home: ctx.world.home,
          env: replayEnvironment(ctx.world, { proxy: ctx.proxy.url, run: ctx.runId }),
          onNotification: (message) => notifications.push(message), onServerRequest() {},
          handleServerRequest(request) { return { error: { code: -32601, message: `unexpected I03 server request ${request.method}` } }; },
        });
        let peerClosed = false;
        const eventsFor = (alias: "A" | "B") => notifications.filter((message) => message.params?.sessionId === ctx.sessionIds[alias]);
        const textFor = (alias: "A" | "B") => JSON.stringify(eventsFor(alias));
        const assertHealthy = () => expect(ctx.proxy.snapshot(ctx.runId).errors).toEqual([]);
        const snapshot = async (alias: "A" | "B") => {
          const reply = await peer.call("session/input/snapshot", { sessionId: ctx.sessionIds[alias] });
          expect(reply.error, JSON.stringify(reply)).toBeUndefined();
          return reply.result;
        };
        const enqueue = async (alias: "A" | "B", turn: string, content: string, inputId: string) => {
          const initial = await snapshot(alias);
          expect(initial.activeRequestId).toBeUndefined();
          expect(initial.items).toEqual([]);
          const offset = notifications.length;
          ctx.identity.arm(ctx.sessionIds[alias]!, turn);
          const receipt = await peer.call("session/input/enqueue", {
            sessionId: ctx.sessionIds[alias], generation: initial.generation,
            commandId: `I03-enqueue-${inputId}`, inputId, content, originalDraft: content,
          });
          expect(receipt.error, JSON.stringify(receipt)).toBeUndefined();
          let requestId: string | undefined;
          await expect.poll(() => {
            assertHealthy();
            const starts = notifications.slice(offset)
              .filter((message) => message.method === "peri/agent_event" && message.params?.sessionId === ctx.sessionIds[alias])
              .map((message) => JSON.parse(message.params.event_json))
              .filter((value) => value.type === "user_input_run_started" && value.value.generation === initial.generation);
            expect(starts.length).toBeLessThanOrEqual(1);
            requestId = starts[0]?.value.request_id;
            return requestId;
          }, { timeout: 30_000 }).toBeTypeOf("string");
          return { alias, inputId, generation: initial.generation as string, requestId: requestId! };
        };
        type Attempt = Awaited<ReturnType<typeof enqueue>>;
        const waitDone = async (attempt: Attempt, stopReason: "cancelled" | "end_turn") => {
          await expect.poll(() => {
            assertHealthy();
            return eventsFor(attempt.alias).filter((message) => message.method === "peri/agent_event_done"
              && message.params?.requestId === attempt.requestId).map((message) => message.params.stopReason);
          }, { timeout: 30_000 }).toEqual([stopReason]);
          await expect.poll(async () => {
            const current = await snapshot(attempt.alias);
            return { active: current.activeRequestId ?? null, items: current.items,
              terminal: current.terminalItems.filter((item: any) => item.inputId === attempt.inputId) };
          }, { timeout: 30_000 }).toMatchObject({ active: null, items: [], terminal: [{ inputId: attempt.inputId, state: "delivered" }] });
          ctx.identity.finish(ctx.sessionIds[attempt.alias]!);
        };
        try {
          const initialized = await peer.call("initialize", { protocolVersion: 1, clientCapabilities: { _meta: { "peri.userInputQueue": true } } });
          expect(initialized.error).toBeUndefined();
          for (const alias of ["A", "B"] as const) {
            const loaded = await peer.call("session/load", { sessionId: ctx.sessionIds[alias], cwd: ctx.world.work, mcpServers: [] });
            expect(loaded.error, JSON.stringify(loaded)).toBeUndefined();
          }
          const firstA = await enqueue("A", "turn-1", "I03_START_CANCEL_A", "00000000-0000-0000-0000-000000000301");
          await ctx.run.waitReached(identity("A"), { timeout: 30_000 });
          const firstB = await enqueue("B", "turn-1", "I03_START_KEEP_B", "00000000-0000-0000-0000-000000000302");
          await ctx.run.waitReached(identity("B"), { timeout: 30_000 });
          await expect.poll(() => textFor("A"), { timeout: 30_000 }).toContain(PREFIX_A);
          await expect.poll(() => textFor("B"), { timeout: 30_000 }).toContain(PREFIX_B);
          expect(textFor("A")).not.toContain(FORBIDDEN_A);
          expect(textFor("B")).not.toContain(DONE_B);
          expect(ctx.proxy.snapshot(ctx.runId).calls.filter((call: any) => call.state === "streaming")).toHaveLength(2);

          peer.notify("session/cancel", { sessionId: ctx.sessionIds.A, generation: firstA.generation, requestId: firstA.requestId });
          await waitDone(firstA, "cancelled");
          await ctx.proxy.waitFor(ctx.runId, (state: any) => state.calls.some((call: any) => call.identity.owner === "A"
            && call.identity.execution === "turn-1" && call.terminal === "client_abort"), { timeout: 30_000 });
          expect((await snapshot("B")).activeRequestId).toBe(firstB.requestId);
          expect(eventsFor("B").filter((message) => message.method === "peri/agent_event_done")).toEqual([]);
          ctx.observe("A_cancel_did_not_cancel_B", { a: firstA, b: firstB, proxy: ctx.proxy.snapshot(ctx.runId) });

          const secondA = await enqueue("A", "turn-2", "I03_START_NEW_A", "00000000-0000-0000-0000-000000000303");
          expect(secondA.requestId).not.toBe(firstA.requestId);
          await ctx.run.waitReached(identity("A", "turn-2"), { timeout: 30_000 });
          // An old targeted Stop arrives after the replacement execution has started.
          peer.notify("session/cancel", { sessionId: ctx.sessionIds.A, generation: firstA.generation, requestId: firstA.requestId });
          // The ordered snapshot roundtrip acknowledges that the notification was processed.
          expect((await snapshot("A")).activeRequestId).toBe(secondA.requestId);
          expect((await snapshot("B")).activeRequestId).toBe(firstB.requestId);
          ctx.run.release(identity("A", "turn-2"));
          await waitDone(secondA, "end_turn");
          expect(textFor("A")).toContain(NEXT_A);
          expect(textFor("B")).not.toContain(NEXT_A);
          expect(textFor("B")).not.toContain(DONE_B);

          ctx.run.release(identity("B"));
          await waitDone(firstB, "end_turn");
          expect(textFor("B")).toContain(DONE_B);
          expect(textFor("A")).not.toContain(DONE_B);
          expect(textFor("A")).not.toContain(PREFIX_B);
          expect(textFor("B")).not.toContain(PREFIX_A);
          expect(JSON.stringify(notifications)).not.toContain(FORBIDDEN_A);
          await ctx.proxy.waitFor(ctx.runId, (state: any) => state.active === 0 && state.calls.length === 3, { timeout: 30_000 });
          expect(ctx.requests).toHaveLength(3);
          for (const observed of ctx.requests) {
            const messages = JSON.stringify(JSON.parse(observed.request.body.toString("utf8")).messages);
            const forbidden = observed.context.owner === "A"
              ? [PREFIX_B, DONE_B, "I03_START_KEEP_B"]
              : [PREFIX_A, NEXT_A, "I03_START_CANCEL_A", "I03_START_NEW_A"];
            for (const marker of forbidden) expect(messages).not.toContain(marker);
          }
          const calls = ctx.proxy.snapshot(ctx.runId).calls;
          expect(calls.map((call: any) => [call.identity.owner, call.identity.execution, call.terminal]).sort()).toEqual([
            ["A", "turn-1", "client_abort"], ["A", "turn-2", "complete"], ["B", "turn-1", "complete"],
          ]);
          ctx.observe("isolated_completion", { calls, notifications });
          for (const alias of ["A", "B"] as const) {
            const closed = await peer.call("session/close", { sessionId: ctx.sessionIds[alias] });
            expect(closed.error).toBeUndefined();
          }
          expect(await peer.close()).toBe(0); peerClosed = true;
          const completionOrder = (left: string[], right: string[]) => JSON.stringify(left).localeCompare(JSON.stringify(right));
          expect(notifications.filter((message) => message.method === "peri/agent_event_done")
            .map((message) => [message.params.sessionId, message.params.requestId, message.params.stopReason]).sort(completionOrder)).toEqual([
            [ctx.sessionIds.A!, firstA.requestId, "cancelled"],
            [ctx.sessionIds.A!, secondA.requestId, "end_turn"],
            [ctx.sessionIds.B!, firstB.requestId, "end_turn"],
          ].sort(completionOrder));
          for (const [alias, expected, forbidden] of [["A", NEXT_A, DONE_B], ["B", DONE_B, NEXT_A]] as const) {
            const tui = await ctx.startTui(alias);
            const screen = await tui.waitFor(ctx.checked((value: string) => value.includes(expected)), `${alias} persisted history visible in real TUI`, 30_000);
            expect(screen).not.toContain(forbidden);
            expect(screen).not.toContain(FORBIDDEN_A);
            const otherSessionMarkers = alias === "A" ? [PREFIX_B, "I03_START_KEEP_B"]
              : [PREFIX_A, "I03_START_CANCEL_A", "I03_START_NEW_A"];
            for (const marker of otherSessionMarkers) expect(screen).not.toContain(marker);
            await tui.save("isolated-history");
            ctx.observe(`${alias}_visible_history`, screen);
            await tui.stop();
          }
        } finally {
          await writeFile(path.join(ctx.world.evidence, "I03-acp-notifications.json"), JSON.stringify(notifications, null, 2));
          if (!peerClosed) {
            await writeFile(path.join(ctx.world.evidence, "I03-acp-failure.json"), await peer.diagnostics());
            await peer.close();
          }
        }
      },
    });
  });
});
