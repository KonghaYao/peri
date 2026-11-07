import { createServer, type ServerResponse } from "node:http";
import { writeFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { ScenarioIdentity } from "../../helpers/llm-replay-identity.js";
import { startStdioAcpFixture } from "../../helpers/stdio-acp-fixture.js";
import {
  checkpointReplayWorld, createReplayWorld, FIXTURE_MODEL_ORIGIN, prepareReplaySessions,
  REPLAY_BINARY, replayEnvironment, ReplayTui, restoreReplayWorld, syntheticRequestEvidence,
} from "../../helpers/llm-replay-tui.js";

const proxyModuleUrl = new URL("../../llm-replay/proxy.mjs", import.meta.url).href;

function answer(text: string): string {
  const events = [
    ["message_start", { type: "message_start", message: { id: "multi-fixture", type: "message", role: "assistant", model: "fixture-model", content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 0 } } }],
    ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }],
    ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } }],
    ["content_block_stop", { type: "content_block_stop", index: 0 }],
    ["message_delta", { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: 5 } }],
    ["message_stop", { type: "message_stop" }],
  ];
  return events.map(([event, data]) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`).join("");
}

describe("真实同后端多会话的录放归属", () => {
  it("相同用户输入以相反请求与响应顺序重放，随后分别在 TUI 加载历史", { timeout: 180_000 }, async () => {
    const { startReplayProxy } = await import(proxyModuleUrl);
    const world = await createReplayWorld();
    console.log(`REPLAY_EVIDENCE=${world.evidence}`);
    const [sessionA, sessionB] = await prepareReplaySessions(world, 2);
    await checkpointReplayWorld(world);
    const mainResponses: ServerResponse[] = [];
    const fixtureErrors: string[] = [];
    const upstream = createServer((request, response) => { void (async () => {
      const buffers: Buffer[] = [];
      for await (const chunk of request) buffers.push(Buffer.from(chunk));
      const body = JSON.parse(Buffer.concat(buffers).toString("utf8"));
      response.writeHead(200, { "content-type": "text/event-stream" });
      if (Array.isArray(body.tools) && body.tools.length) {
        if (!JSON.stringify(body.messages).includes("IDENTICAL_USER_PROMPT")) throw new Error("unexpected input");
        mainResponses.push(response);
      } else response.end(answer("Fixture suggestion"));
    })().catch((error) => { fixtureErrors.push(String(error)); response.end(); }); });
    await new Promise<void>((resolve) => upstream.listen(0, "127.0.0.1", resolve));
    const address = upstream.address();
    if (!address || typeof address === "string") throw new Error("fixture did not bind");
    const upstreamUrl = `http://127.0.0.1:${address.port}`;
    const closeUpstream = () => new Promise<void>((resolve) => { upstream.closeAllConnections(); upstream.close(() => resolve()); });
    const cassetteDir = path.join(world.root, "cassette");
    let proxy: any;
    let peer: Awaited<ReturnType<typeof startStdioAcpFixture>> | undefined;
    let tui: ReplayTui | undefined;
    let currentRun = "";
    try {
      for (const mode of ["record", "replay"] as const) {
        if (mode === "replay") await restoreReplayWorld(world);
        currentRun = `${mode}-sessions`;
        const identities = new ScenarioIdentity([{ live: sessionA!, canonical: "A" }, { live: sessionB!, canonical: "B" }]);
        const key = (owner: string) => ({ owner, execution: "turn-1", call: "reason:1", attempt: 1, purpose: "reason" });
        proxy = await startReplayProxy({ mode, cassetteDir,
          ...(mode === "record" ? { upstreams: [{ logicalOrigin: FIXTURE_MODEL_ORIGIN, transportOrigin: upstreamUrl }] } : {}),
          provenance: { source: "local-fixture-model", scenario: "shared-backend-two-sessions", realLlm: false },
        });
        const run = proxy.createRun(currentRun, {
          resolveContext: identities.resolve,
          onRequest: syntheticRequestEvidence(world, mode),
          ...(mode === "replay" ? { gates: [{ identity: key("A"), afterBytes: 0 }, { identity: key("B"), afterBytes: 0 }] } : {}),
        });
        const notifications: any[] = [];
        peer = await startStdioAcpFixture({ binary: REPLAY_BINARY, cwd: world.work, home: world.home,
          env: replayEnvironment(world, { proxy: proxy.url, run: currentRun }),
          onNotification: (message) => notifications.push(message), onServerRequest() {},
          handleServerRequest(request) { return { error: { code: -32601, message: `unexpected server request ${request.method}` } }; },
        });
        const initialized = await peer.call("initialize", { protocolVersion: 1, clientCapabilities: { _meta: { "peri.userInputQueue": true } } });
        expect(initialized.error).toBeUndefined();
        for (const sessionId of [sessionA, sessionB]) {
          const loaded = await peer.call("session/load", { sessionId, cwd: world.work, mcpServers: [] });
          expect(loaded.error, JSON.stringify(loaded)).toBeUndefined();
        }
        const inputIds = new Map([
          [sessionA!, "00000000-0000-0000-0000-000000000001"],
          [sessionB!, "00000000-0000-0000-0000-000000000002"],
        ]);
        const generations = new Map<string, string>();
        const enqueue = async (sessionId: string) => {
          const before = await peer!.call("session/input/snapshot", { sessionId });
          expect(before.error).toBeUndefined();
          expect(before.result.activeRequestId).toBeUndefined();
          expect(before.result.items).toEqual([]);
          generations.set(sessionId, before.result.generation);
          identities.arm(sessionId, "turn-1");
          const receipt = await peer!.call("session/input/enqueue", {
            sessionId, generation: before.result.generation,
            commandId: `enqueue-${inputIds.get(sessionId)}`, inputId: inputIds.get(sessionId),
            content: "IDENTICAL_USER_PROMPT", originalDraft: "IDENTICAL_USER_PROMPT",
          });
          expect(receipt.error, JSON.stringify(receipt)).toBeUndefined();
        };
        const waitDone = async (sessionId: string) => {
          // Enqueue acknowledges admission only. Pair the real run-start notification with its done event.
          let requestId: string | undefined;
          await expect.poll(() => {
            const starts = notifications.filter((message) => message.method === "peri/agent_event" && message.params?.sessionId === sessionId)
              .map((message) => JSON.parse(message.params.event_json))
              .filter((event) => event.type === "user_input_run_started" && event.value.generation === generations.get(sessionId));
            expect(starts.length).toBeLessThanOrEqual(1);
            requestId = starts[0]?.value.request_id;
            return requestId ? notifications.find((message) => message.method === "peri/agent_event_done"
              && message.params?.sessionId === sessionId && message.params?.requestId === requestId) : undefined;
          }, { timeout: 30_000 }).toBeDefined();
          const done = notifications.find((message) => message.method === "peri/agent_event_done"
            && message.params?.sessionId === sessionId && message.params?.requestId === requestId);
          expect(done.params.stopReason).toBe("end_turn");
          await expect.poll(async () => {
            const current = await peer!.call("session/input/snapshot", { sessionId, generation: generations.get(sessionId) });
            expect(current.error).toBeUndefined();
            return { ...current.result, activeRequestId: current.result.activeRequestId ?? null };
          }, { timeout: 30_000 }).toMatchObject({ activeRequestId: null, items: [], terminalItems: [{ inputId: inputIds.get(sessionId), state: "delivered" }] });
        };
        if (mode === "record") {
          await enqueue(sessionA!);
          await proxy.waitFor(currentRun, (state: any) => state.calls.some((call: any) => call.identity.owner === "A"), { timeout: 30_000 });
          await expect.poll(() => mainResponses.length, { timeout: 30_000 }).toBe(1);
          await enqueue(sessionB!);
          await expect.poll(() => mainResponses.length, { timeout: 30_000 }).toBe(2);
          mainResponses[1]!.end(answer("SESSION_B_ONLY"));
          await waitDone(sessionB!);
          mainResponses[0]!.end(answer("SESSION_A_ONLY"));
          await waitDone(sessionA!);
        } else {
          await enqueue(sessionB!);
          await run.waitReached(key("B"), { timeout: 30_000 });
          await enqueue(sessionA!);
          await run.waitReached(key("A"), { timeout: 30_000 });
          run.release(key("A"));
          await waitDone(sessionA!);
          run.release(key("B"));
          await waitDone(sessionB!);
        }
        await proxy.waitFor(currentRun, (state: any) => state.active === 0 && state.calls.length === 2, { timeout: 30_000 });
        const sessionUpdates = (sessionId: string) => JSON.stringify(notifications.filter((message) => message.params?.sessionId === sessionId));
        expect(sessionUpdates(sessionA!)).toContain("SESSION_A_ONLY");
        expect(sessionUpdates(sessionA!)).not.toContain("SESSION_B_ONLY");
        expect(sessionUpdates(sessionB!)).toContain("SESSION_B_ONLY");
        expect(sessionUpdates(sessionB!)).not.toContain("SESSION_A_ONLY");
        await writeFile(path.join(world.evidence, `${mode}-notifications.json`), JSON.stringify(notifications, null, 2));
        for (const sessionId of [sessionA, sessionB]) {
          identities.finish(sessionId!);
          const closed = await peer.call("session/close", { sessionId });
          expect(closed.error).toBeUndefined();
        }
        expect(await peer.close()).toBe(0); peer = undefined;
        const summary = await proxy.completeRun(currentRun);
        expect(summary.calls.filter((call: any) => call.identity.purpose === "reason")).toHaveLength(2);
        if (mode === "replay") expect(summary.upstreamCalls).toBe(0);
        await writeFile(path.join(world.evidence, `${mode}-proxy.json`), JSON.stringify(summary, null, 2));
        for (const [sessionId, own, other] of [[sessionA, "SESSION_A_ONLY", "SESSION_B_ONLY"], [sessionB, "SESSION_B_ONLY", "SESSION_A_ONLY"]]) {
          tui = await ReplayTui.start(world, { label: `${mode}-${own}`, sessionId: sessionId!, proxy: proxy.url, run: currentRun });
          const screen = await tui.waitFor((value) => value.includes(own!), "persisted session visible in TUI");
          expect(screen).not.toContain(other);
          await tui.save("history");
          await tui.stop(); tui = undefined;
        }
        expect(proxy.snapshot(currentRun).errors).toEqual([]);
        if (mode === "record") { expect(fixtureErrors).toEqual([]); await proxy.seal(); await closeUpstream(); }
        await proxy.close(); proxy = undefined;
      }
    } finally {
      await tui?.save("failure").catch(() => {});
      await tui?.stop();
      if (peer) await writeFile(path.join(world.evidence, "failure-acp.json"), JSON.stringify(await peer.diagnostics(), null, 2));
      await peer?.close();
      if (proxy) { await writeFile(path.join(world.evidence, "failure-proxy.json"), JSON.stringify(proxy.snapshot(currentRun), null, 2)); await proxy.close(); }
      await closeUpstream();
    }
  });
});
