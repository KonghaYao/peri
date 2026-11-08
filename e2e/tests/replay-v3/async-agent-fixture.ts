import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { expect } from "vitest";
import type { ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import type { ReplayTui, ReplayWorld } from "../../helpers/llm-replay-tui.js";
import { AgentIdentities, configureAgent, identity, type CallIdentity, type ChildAnchor, type Reply } from "../replay-v2/agents-fixture.js";
export { AgentIdentities, assertChildTrace, assertFile, assertMissingFile, childIdFromResult, completed,
  contentText, delegate, expectResult, healthy, identity, persistedTrace, read, resultText, text, tool, tools, write,
  type CallIdentity, type ChildAnchor, type Reply } from "../replay-v2/agents-fixture.js";

export const CONFIG_VERSION = "async-agent-v3.1";
export async function configureBackground(world: ReplayWorld) { await configureAgent(world); }
export async function configureForcedBackground(world: ReplayWorld) {
  await configureAgent(world);
  const file = path.join(world.home, ".peri/settings.json");
  const settings = JSON.parse(await readFile(file, "utf8"));
  settings.config.betas = { "full-async-tools": true };
  await writeFile(file, JSON.stringify(settings));
}

interface HttpFailure { status: number; error: string }
const sse = (type: string, data: unknown) => `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
function encode(reply: Reply, call: CallIdentity): string {
  let result = sse("message_start", { type: "message_start", message: {
    id: [call.owner, call.execution, call.call].join("-"), type: "message", role: "assistant", model: "fixture-model",
    content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 30, output_tokens: 0 },
  } });
  let index = 0;
  if (reply.text !== undefined) {
    result += sse("content_block_start", { type: "content_block_start", index, content_block: { type: "text", text: "" } });
    result += sse("content_block_delta", { type: "content_block_delta", index, delta: { type: "text_delta", text: reply.text } });
    result += sse("content_block_stop", { type: "content_block_stop", index: index++ });
  }
  for (const use of reply.tools ?? []) {
    result += sse("content_block_start", { type: "content_block_start", index, content_block: { type: "tool_use", id: use.id, name: use.name, input: {} } });
    result += sse("content_block_delta", { type: "content_block_delta", index, delta: { type: "input_json_delta", partial_json: JSON.stringify(use.input) } });
    result += sse("content_block_stop", { type: "content_block_stop", index: index++ });
  }
  return result + sse("message_delta", { type: "message_delta", delta: { stop_reason: reply.tools?.length ? "tool_use" : "end_turn", stop_sequence: null }, usage: { output_tokens: 10 } })
    + sse("message_stop", { type: "message_stop" });
}

/** The request identity comes from the trusted local proxy, never from model text or arrival order. */
export async function backgroundProvider(script: (call: CallIdentity, body: any) => Reply | HttpFailure) {
  const pending = new Map<string, CallIdentity>(), errors: string[] = [];
  const hash = (raw: Buffer) => createHash("sha256").update(raw).digest("hex");
  const server = createServer((request, response) => { void (async () => {
    const parts: Buffer[] = [];
    for await (const part of request) parts.push(Buffer.from(part));
    const raw = Buffer.concat(parts), key = hash(raw), call = pending.get(key);
    if (!call) throw new Error("uncorrelated async-agent provider request");
    pending.delete(key);
    if (request.headers["x-peri-replay-context"]) throw new Error("private context leaked upstream");
    const reply = call.purpose === "prediction" ? { text: "Review the result" } : script(call, JSON.parse(raw.toString("utf8")));
    if ("status" in reply) {
      response.writeHead(reply.status, { "content-type": "application/json" });
      response.end(JSON.stringify({ type: "error", error: { type: "authentication_error", message: reply.error } }));
    } else {
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(encode(reply, call));
    }
  })().catch((error) => { errors.push(String(error)); response.writeHead(400); response.end(String(error)); }); });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("async-agent provider failed to bind");
  return { url: `http://127.0.0.1:${address.port}`, errors,
    correlate(call: CallIdentity, raw: Buffer) {
      const key = hash(raw);
      if (pending.has(key)) throw new Error("ambiguous exact concurrent model bodies");
      pending.set(key, structuredClone(call));
    },
    async close() { server.closeAllConnections(); await new Promise<void>((resolve) => server.close(() => resolve())); },
  };
}

export function unexpected(call: CallIdentity): never { throw new Error(`unexpected async-agent model call: ${JSON.stringify(call)}`); }
export function options(context: ReplayScenarioContext, anchors: ChildAnchor[], gates: CallIdentity[] = []) {
  const actors = new AgentIdentities(context, anchors);
  return { actors, proxy: { resolveContext: actors.resolve, onRequest: actors.onRequest,
    gates: gates.map((call) => ({ identity: call, afterBytes: 0 })) } };
}
export function rootCall(call: string, execution = "turn-1") { return identity("root", call, execution); }
export async function waitCall(context: ReplayScenarioContext, expected: CallIdentity, terminal = "complete") {
  return context.waitForCalls((state) => state.calls.some((call: any) => call.identity.owner === expected.owner
    && call.identity.execution === expected.execution && call.identity.call === expected.call && call.terminal === terminal));
}
export function requestBody(context: ReplayScenarioContext, expected: CallIdentity): any {
  const requests = context.requests.filter((request) => request.context.owner === expected.owner
    && request.context.execution === expected.execution && request.context.call === expected.call
    && request.context.purpose === expected.purpose && request.context.attempt === expected.attempt);
  expect(requests, `exact request ${JSON.stringify(expected)}`).toHaveLength(1);
  return JSON.parse(requests[0]!.request.body.toString("utf8"));
}
export async function oscStates(tui: ReplayTui, label: string): Promise<string[]> {
  await tui.save(label);
  const raw = await readFile(path.join(tui.world.evidence, `${tui.label}-${label}.pty`), "utf8");
  return [...raw.matchAll(/\x1b\]7501;state=([a-z]+)(?::[^\x07\x1b]*)?(?:\x07|\x1b\\)/gu)].map((match) => match[1]!);
}
export async function assertOsc(tui: ReplayTui, state: string, label: string) {
  await expect.poll(async () => (await oscStates(tui, label)).at(-1), { timeout: 10_000 }).toBe(state);
}
export function threadState(context: ReplayScenarioContext, thread: string): any {
  const db = new DatabaseSync(path.join(context.world.home, ".peri/threads/threads.db"), { readOnly: true });
  try { return db.prepare("SELECT id, parent_thread_id, agent_status, work_state FROM threads WHERE id = ?").get(thread); }
  finally { db.close(); }
}
export function rootPayloads(context: ReplayScenarioContext): any[] {
  const db = new DatabaseSync(path.join(context.world.home, ".peri/threads/threads.db"), { readOnly: true });
  try { return db.prepare("SELECT content FROM messages WHERE thread_id = ? ORDER BY rowid").all(context.sessionIds.root!)
    .map((row: any) => JSON.parse(row.content)); }
  finally { db.close(); }
}
export function assertCanonicalResultOnce(context: ReplayScenarioContext, report: string) {
  const reminders = rootPayloads(context).filter((payload) => payload.type !== "message" && JSON.stringify(payload).includes(report));
  expect(reminders, `one canonical async result for ${report}`).toHaveLength(1);
}
export function countRootUserInputs(context: ReplayScenarioContext) {
  return rootPayloads(context).filter((payload) => payload.type === "message" && payload.message.role === "user").length;
}
