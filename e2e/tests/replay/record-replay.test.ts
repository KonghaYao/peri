import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { writeFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { ScenarioIdentity } from "../../helpers/llm-replay-identity.js";
import {
  checkpointReplayWorld, createReplayWorld, FIXTURE_MODEL_ORIGIN, prepareReplaySessions,
  readReplayFile, ReplayTui, restoreReplayWorld, syntheticRequestEvidence, type ReplayWorld,
} from "../../helpers/llm-replay-tui.js";

const proxyModuleUrl = new URL("../../llm-replay/proxy.mjs", import.meta.url).href;

function sse(type: string, data: object): string { return `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`; }
function startMessage(): string {
  return sse("message_start", { type: "message_start", message: {
    id: "fixture-message", type: "message", role: "assistant", model: "fixture-model", content: [],
    stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 0 },
  } });
}
function textPrefix(text: string): string {
  return startMessage() + sse("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } })
    + sse("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } });
}
function textResponse(text: string): string {
  return textPrefix(text) + sse("content_block_stop", { type: "content_block_stop", index: 0 })
    + sse("message_delta", { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: 5 } })
    + sse("message_stop", { type: "message_stop" });
}
function writeResponse(file: string): string {
  return startMessage()
    + sse("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: "fixture-write-1", name: "Write", input: {} } })
    + sse("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: JSON.stringify({ file_path: file, content: "RECORDED_TOOL_EFFECT\n" }) } })
    + sse("content_block_stop", { type: "content_block_stop", index: 0 })
    + sse("message_delta", { type: "message_delta", delta: { stop_reason: "tool_use", stop_sequence: null }, usage: { output_tokens: 5 } })
    + sse("message_stop", { type: "message_stop" });
}

async function fixtureModel(world: ReplayWorld, scenario: "write" | "cancel") {
  const requests: any[] = [];
  const errors: string[] = [];
  const server = createServer((request, response) => { void handle(request, response).catch((error) => {
    errors.push(String(error)); response.writeHead(500); response.end(String(error));
  }); });
  async function handle(request: IncomingMessage, response: ServerResponse) {
    const buffers: Buffer[] = [];
    for await (const chunk of request) buffers.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(buffers).toString("utf8"));
    requests.push(body);
    if (request.headers["x-peri-replay-context"] || request.headers["x-peri-replay-target"]) throw new Error("proxy leaked replay metadata upstream");
    response.writeHead(200, { "content-type": "text/event-stream" });
    const hasTools = Array.isArray(body.tools) && body.tools.length > 0;
    if (!hasTools) { response.end(textResponse("Fixture suggestion")); return; }
    const contents = JSON.stringify(body.messages);
    if (scenario === "write") {
      if (!contents.includes("RECORD_WRITE_CASE")) throw new Error("unexpected write scenario input");
      const completed = body.messages.some((message: any) => Array.isArray(message.content) && message.content.some((block: any) =>
        block.type === "tool_result" && block.tool_use_id === "fixture-write-1"));
      response.end(completed ? textResponse("REPLAY_WRITE_DONE") : writeResponse(path.join(world.work, "result.txt")));
      return;
    }
    if (contents.includes("CONTINUE_AFTER_CANCEL")) { response.end(textResponse("CONTINUATION_VISIBLE")); return; }
    if (!contents.includes("CANCEL_STREAM_CASE")) throw new Error("unexpected cancel scenario input");
    // The proxy withholds this suffix at the exact prefix boundary until a real client cancel.
    response.end(textPrefix("PREFIX_VISIBLE_BEFORE_CANCEL")
      + sse("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "FORBIDDEN_AFTER_CANCEL" } })
      + sse("content_block_stop", { type: "content_block_stop", index: 0 })
      + sse("message_delta", { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: 5 } })
      + sse("message_stop", { type: "message_stop" }));
  }
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("fixture model did not bind");
  return {
    url: `http://127.0.0.1:${address.port}`, requests, errors,
    close: () => new Promise<void>((resolve, reject) => { server.closeAllConnections(); server.close((error) => error ? reject(error) : resolve()); }),
  };
}

async function saveSummary(world: ReplayWorld, mode: string, value: unknown): Promise<void> {
  await writeFile(path.join(world.evidence, `${mode}-proxy.json`), JSON.stringify(value, null, 2));
}

function healthyScreen(proxy: any, run: string, predicate: (screen: string) => boolean) {
  return (screen: string) => {
    const errors = proxy.snapshot(run).errors;
    if (errors.length) throw new Error(`replay proxy rejected a request: ${JSON.stringify(errors)}`);
    return predicate(screen);
  };
}

describe("真实 Peri TUI 的模型 HTTP 录制与离线重放", () => {
  it("录制本地 fixture 模型，关闭上游后重放真实 Write 和 TUI 结果", { timeout: 180_000 }, async () => {
    const { startReplayProxy } = await import(proxyModuleUrl);
    const world = await createReplayWorld();
    console.log(`REPLAY_EVIDENCE=${world.evidence}`);
    const [sessionId] = await prepareReplaySessions(world);
    await checkpointReplayWorld(world);
    const upstream = await fixtureModel(world, "write");
    const cassetteDir = path.join(world.root, "cassette");
    let tui: ReplayTui | undefined;
    let proxy: any;
    let currentRun = "";
    try {
      for (const mode of ["record", "replay"] as const) {
        if (mode === "replay") await restoreReplayWorld(world);
        const identity = new ScenarioIdentity([{ live: sessionId!, canonical: "root" }]);
        const runId = `${mode}-write`;
        currentRun = runId;
        proxy = await startReplayProxy({ mode, cassetteDir,
          ...(mode === "record" ? { upstreams: [{ logicalOrigin: FIXTURE_MODEL_ORIGIN, transportOrigin: upstream.url }] } : {}),
          provenance: { source: "local-fixture-model", scenario: "real-tui-write", realLlm: false },
        });
        proxy.createRun(runId, { resolveContext: identity.resolve, onRequest: syntheticRequestEvidence(world, mode) });
        identity.arm(sessionId!, "turn-1");
        tui = await ReplayTui.start(world, { label: mode, sessionId: sessionId!, proxy: proxy.url, run: runId });
        await tui.prompt("RECORD_WRITE_CASE");
        const finalScreen = await tui.waitFor(healthyScreen(proxy, runId, (screen) => screen.includes("REPLAY_WRITE_DONE") && /✓\s+Write/.test(screen) && screen.includes("Brewed for")), "actual Write card and completed turn", 60_000);
        expect(finalScreen).toContain("result.txt");
        expect(await readReplayFile(world, "result.txt")).toBe("RECORDED_TOOL_EFFECT\n");
        await tui.save("write-completed");
        await proxy.waitFor(runId, (state: any) => state.active === 0 && state.calls.length === 2, { timeout: 10_000 });
        identity.finish(sessionId!);
        const summary = await proxy.completeRun(runId);
        expect(summary.errors).toEqual([]);
        expect(summary.active).toBe(0);
        expect(summary.calls.filter((call: any) => call.identity.purpose === "reason")).toHaveLength(2);
        if (mode === "replay") expect(summary.upstreamCalls).toBe(0);
        await saveSummary(world, mode, summary);
        await tui.stop(); tui = undefined;
        if (mode === "record") {
          expect(upstream.errors).toEqual([]);
          await proxy.seal();
          await upstream.close();
        }
        await proxy.close(); proxy = undefined;
      }
    } finally {
      await tui?.save("failure").catch(() => {});
      await tui?.stop();
      if (proxy) { await saveSummary(world, "failure", proxy.snapshot(currentRun)); await proxy.close(); }
      await upstream.close().catch(() => {});
    }
  });

  it("真实流式前缀可见后取消，重放等待真实取消并能继续输入", { timeout: 180_000 }, async () => {
    const { startReplayProxy } = await import(proxyModuleUrl);
    const world = await createReplayWorld();
    console.log(`REPLAY_EVIDENCE=${world.evidence}`);
    const [sessionId] = await prepareReplaySessions(world);
    await checkpointReplayWorld(world);
    const upstream = await fixtureModel(world, "cancel");
    const cassetteDir = path.join(world.root, "cassette");
    let tui: ReplayTui | undefined;
    let proxy: any;
    let currentRun = "";
    try {
      for (const mode of ["record", "replay"] as const) {
        if (mode === "replay") await restoreReplayWorld(world);
        const identity = new ScenarioIdentity([{ live: sessionId!, canonical: "root" }]);
        currentRun = `${mode}-cancel`;
        proxy = await startReplayProxy({ mode, cassetteDir,
          ...(mode === "record" ? { upstreams: [{ logicalOrigin: FIXTURE_MODEL_ORIGIN, transportOrigin: upstream.url }] } : {}),
          provenance: { source: "local-fixture-model", scenario: "real-tui-cancel", realLlm: false },
        });
        const cancelIdentity = { owner: "root", execution: "turn-1", call: "reason:1", attempt: 1, purpose: "reason" };
        const continueIdentity = { ...cancelIdentity, execution: "turn-2" };
        const prefixBytes = Buffer.byteLength(textPrefix("PREFIX_VISIBLE_BEFORE_CANCEL"));
        const run = proxy.createRun(currentRun, {
          resolveContext: identity.resolve, onRequest: syntheticRequestEvidence(world, mode),
          gates: [{ identity: cancelIdentity, afterBytes: prefixBytes }, { identity: continueIdentity, afterBytes: 0 }],
        });
        identity.arm(sessionId!, "turn-1");
        tui = await ReplayTui.start(world, { label: mode, sessionId: sessionId!, proxy: proxy.url, run: currentRun });
        await tui.prompt("CANCEL_STREAM_CASE");
        await run.waitReached(cancelIdentity, { timeout: 15_000 });
        const prefix = await tui.waitFor(healthyScreen(proxy, currentRun, (screen) => screen.includes("PREFIX_VISIBLE_BEFORE_CANCEL")), "stream prefix rendered before cancel");
        expect(prefix).not.toContain("Brewed for");
        expect(prefix).not.toContain("FORBIDDEN_AFTER_CANCEL");
        await tui.save("prefix-visible");
        await tui.key("C-c");
        await proxy.waitFor(currentRun, (state: any) => state.calls.some((call: any) => call.identity.execution === "turn-1" && call.terminal === "client_abort"), { timeout: 15_000 });
        await tui.waitFor((screen) => screen.includes("Brewed for"), "cancelled turn leaves loading in TUI", 15_000);
        identity.finish(sessionId!);
        identity.arm(sessionId!, "turn-2");
        await tui.prompt("CONTINUE_AFTER_CANCEL");
        await run.waitReached(continueIdentity, { timeout: 15_000 });
        await tui.waitFor((screen) => screen.includes("CONTINUE_AFTER_CANCEL") && !screen.includes("Brewed for"), "new turn replaces the previous completed footer");
        run.release(continueIdentity);
        const finalScreen = await tui.waitFor(healthyScreen(proxy, currentRun, (screen) => screen.includes("CONTINUATION_VISIBLE") && screen.includes("Brewed for")), "new completed reply after cancelled stream");
        expect(finalScreen).not.toContain("REPLAY-UNMATCHED-STEP");
        expect(finalScreen).not.toContain("FORBIDDEN_AFTER_CANCEL");
        await tui.save("continued");
        await proxy.waitFor(currentRun, (state: any) => state.active === 0 && state.calls.length === 2, { timeout: 10_000 });
        identity.finish(sessionId!);
        const summary = await proxy.completeRun(currentRun);
        expect(summary.calls.filter((call: any) => call.terminal === "client_abort")).toHaveLength(1);
        expect(summary.calls.filter((call: any) => call.identity.purpose === "reason")).toHaveLength(2);
        if (mode === "replay") expect(summary.upstreamCalls).toBe(0);
        await saveSummary(world, mode, summary);
        await tui.stop(); tui = undefined;
        if (mode === "record") { await proxy.seal(); await upstream.close(); }
        await proxy.close(); proxy = undefined;
      }
    } finally {
      await tui?.save("failure").catch(() => {});
      await tui?.stop();
      if (proxy) { await saveSummary(world, "failure", proxy.snapshot(currentRun)); await proxy.close(); }
      await upstream.close().catch(() => {});
    }
  });
});
