import { createServer } from "node:http";
import { access, copyFile, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { expect } from "vitest";
import type { ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import { replayEnvironment, REPLAY_BINARY, type ReplayTui, type ReplayWorld } from "../../helpers/llm-replay-tui.js";
import { readReplayStore, replayStorePath } from "../../helpers/replay-isolation.js";
import { startStdioAcpFixture } from "../../helpers/stdio-acp-fixture.js";

type Reply = { text: string; tail?: string } | { tool: { id: string; name: string; input: object } }
  | { error: { status: number; type: string; message: string } };

function event(type: string, data: object): string { return `event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`; }
export function responsePrefix(text: string): string {
  return event("message_start", { message: { id: "session-store-fixture", type: "message", role: "assistant", model: "fixture-model",
    content: [], stop_reason: null, usage: { input_tokens: 24, output_tokens: 0 } } })
    + event("content_block_start", { index: 0, content_block: { type: "text", text: "" } })
    + event("content_block_delta", { index: 0, delta: { type: "text_delta", text } });
}

function responseBody(reply: Exclude<Reply, { error: unknown }>): string {
  const content = "text" in reply ? responsePrefix(reply.text)
    + (reply.tail ? event("content_block_delta", { index: 0, delta: { type: "text_delta", text: reply.tail } }) : "")
    : event("message_start", { message: { id: "session-store-fixture", type: "message", role: "assistant", model: "fixture-model", content: [],
      stop_reason: null, usage: { input_tokens: 24, output_tokens: 0 } } })
      + event("content_block_start", { index: 0, content_block: { type: "tool_use", id: reply.tool.id, name: reply.tool.name, input: {} } })
      + event("content_block_delta", { index: 0, delta: { type: "input_json_delta", partial_json: JSON.stringify(reply.tool.input) } });
  return content + event("content_block_stop", { index: 0 })
    + event("message_delta", { delta: { stop_reason: "tool" in reply ? "tool_use" : "end_turn" }, usage: { output_tokens: 12 } })
    + event("message_stop", {});
}

export async function sessionModel(replyFor: (body: any, index: number) => Reply) {
  const errors: string[] = [];
  let count = 0;
  const server = createServer((request, response) => { void (async () => {
    const chunks: Buffer[] = []; for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    if (!body.tools?.length) throw new Error("unexpected auxiliary model request");
    if (request.headers["x-peri-replay-context"] || request.headers["x-peri-replay-target"]) throw new Error("private route leaked upstream");
    const reply = replyFor(body, ++count);
    if ("error" in reply) {
      response.writeHead(reply.error.status, { "content-type": "application/json" });
      response.end(JSON.stringify({ type: "error", error: { type: reply.error.type, message: reply.error.message } }));
    } else {
      response.writeHead(200, { "content-type": "text/event-stream" }); response.end(responseBody(reply));
    }
  })().catch((error) => { errors.push(String(error)); if (!response.headersSent) response.writeHead(500); response.end(String(error)); }); });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address(); if (!address || typeof address === "string") throw new Error("fixture failed to bind");
  return { url: `http://127.0.0.1:${address.port}`, errors,
    close: () => new Promise<void>((resolve) => { server.closeAllConnections(); server.close(() => resolve()); }) };
}

export function bodyOf(request: any): any { return JSON.parse(request.request.body.toString("utf8")); }
export function lastUserText(body: any): string {
  return (body.messages ?? []).filter((message: any) => message.role === "user")
    .flatMap((message: any) => typeof message.content === "string" ? [message.content]
      : (message.content ?? []).filter((block: any) => block.type === "text").map((block: any) => block.text))
    .filter((text: string) => !text.trimStart().startsWith("<system-reminder")).at(-1) ?? "";
}
export function results(body: any): any[] {
  return (body.messages ?? []).flatMap((message: any) => Array.isArray(message.content) ? message.content : [])
    .filter((block: any) => block.type === "tool_result");
}
export function identity(owner: string, execution: string) { return { owner, execution, call: "reason:1", attempt: 1, purpose: "reason" }; }

export async function turn(ctx: ReplayScenarioContext, tui: ReplayTui, alias: string, execution: string,
  prompt: string, answer: string, calls: number): Promise<string> {
  const id = ctx.sessionIds[alias] ?? ctx.identity.ownerBindings().find((owner) => owner.canonical === alias)?.live;
  if (!id) throw new Error(`unknown test session ${alias}`);
  ctx.identity.arm(id, execution); await tui.prompt(prompt);
  const screen = await tui.waitFor(ctx.checked((value: string) => value.includes(answer) && value.includes("Brewed for")), `${execution} completed`);
  await ctx.waitForCalls((snapshot) => snapshot.active === 0 && snapshot.calls.length === calls);
  ctx.identity.finish(id); await tui.save(execution); ctx.observe(`${alias}-${execution}-screen`, screen);
  return screen;
}

export function stored(world: ReplayWorld, sessionId: string): string[] {
  const store = readReplayStore(world); expect(store.integrity).toBe("ok");
  expect(store.threads.some((thread) => thread.id === sessionId)).toBe(true);
  return store.messages.filter((message) => message.thread_id === sessionId).map((message) => message.content);
}

/** Auxiliary worlds are removed too, but their real process/store evidence belongs to the case. */
export async function preserveAndRemoveAuxiliaryWorld(ctx: ReplayScenarioContext, world: ReplayWorld, label: string): Promise<void> {
  try {
    const directory = path.join(ctx.world.evidence, label);
    await mkdir(directory, { recursive: true });
    const logs = path.join(world.home, ".peri", "logs");
    const names = (await readdir(logs)).filter((name) => /^stdio-process-.*\.json$/.test(name));
    expect(names.length, "auxiliary ACP process exit evidence").toBeGreaterThan(0);
    const acpProcesses = [];
    for (const name of names) {
      const outcome = JSON.parse(await readFile(path.join(logs, name), "utf8"));
      await copyFile(path.join(logs, name), path.join(directory, name));
      expect(outcome.exitCode).toBe(0); expect(outcome.signal).toBeNull(); expect(outcome.forcedCleanup).toBe(false);
      acpProcesses.push(outcome);
    }
    const store = readReplayStore(world); expect(store.integrity).toBe("ok");
    await writeFile(path.join(directory, "store-evidence.json"), `${JSON.stringify(store, null, 2)}\n`);
    ctx.observe(`auxiliary-world-${label}`, { root: world.root, store, acpProcesses });
  } finally {
    await rm(world.root, { recursive: true, force: true });
    await expect(access(world.root)).rejects.toMatchObject({ code: "ENOENT" });
    ctx.observe(`auxiliary-world-${label}-cleanup`, { temporaryDataRemoved: true });
  }
}

export async function sessionPeer(world: ReplayWorld, route: { proxy: string; run: string }) {
  const notifications: any[] = [];
  const peer = await startStdioAcpFixture({ binary: REPLAY_BINARY, cwd: world.work, home: world.home,
    env: replayEnvironment(world, route), dbPath: replayStorePath(world),
    onNotification: (message) => notifications.push(message), onServerRequest() {},
    handleServerRequest(request) { return { error: { code: -32601, message: `unexpected request: ${request.method}` } }; },
  });
  try {
    const init = await peer.call("initialize", { protocolVersion: 1, clientCapabilities: { _meta: { "peri.userInputQueue": true } } });
    expect(init.error, JSON.stringify(init)).toBeUndefined();
    return { ...peer, notifications };
  } catch (error) { await peer.close(); throw error; }
}
