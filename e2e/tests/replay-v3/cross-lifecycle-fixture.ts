import { createHash } from "node:crypto";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { DatabaseSync } from "node:sqlite";
import { expect } from "vitest";
import type { ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import { replayStorePath } from "../../helpers/replay-isolation.js";
import { AgentIdentities, type CallIdentity, type ChildAnchor, type Reply } from "../replay-v2/agents-fixture.js";

interface ProviderReply extends Reply { error?: string }
const event = (type: string, data: object) => `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
const hash = (body: Buffer) => createHash("sha256").update(body).digest("hex");

function wire(call: CallIdentity, reply: Reply): string {
  let result = event("message_start", { type: "message_start", message: {
    id: `${call.owner}-${call.execution}-${call.call}`, type: "message", role: "assistant", model: "fixture-model",
    content: [], stop_reason: null, usage: { input_tokens: 30, output_tokens: 0 },
  } });
  const blocks = [...(reply.text === undefined ? [] : [{ type: "text", text: reply.text }]),
    ...(reply.tools ?? []).map((tool) => ({ type: "tool_use", ...tool }))];
  blocks.forEach((block: any, index) => {
    result += event("content_block_start", { type: "content_block_start", index, content_block: block.type === "text"
      ? { type: "text", text: "" } : { type: "tool_use", id: block.id, name: block.name, input: {} } });
    result += event("content_block_delta", { type: "content_block_delta", index, delta: block.type === "text"
      ? { type: "text_delta", text: block.text } : { type: "input_json_delta", partial_json: JSON.stringify(block.input) } });
    result += event("content_block_stop", { type: "content_block_stop", index });
  });
  return result + event("message_delta", { type: "message_delta", delta: { stop_reason: reply.tools?.length ? "tool_use" : "end_turn", stop_sequence: null }, usage: { output_tokens: 20 } })
    + event("message_stop", { type: "message_stop" });
}

/** Fixture responses replace only the model; tool, mailbox, compact and persistence remain real. */
export async function crossProvider(script: (identity: CallIdentity, body: any) => ProviderReply) {
  const pending = new Map<string, CallIdentity>();
  const errors: string[] = [];
  const server = createServer((request, response) => { void handle(request, response).catch((error) => {
    errors.push(String(error));
    if (!response.headersSent) response.writeHead(500, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: String(error) }));
  }); });
  async function handle(request: IncomingMessage, response: ServerResponse) {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const raw = Buffer.concat(chunks), key = hash(raw), identity = pending.get(key);
    if (!identity) throw new Error("cross-domain request lacks exact identity correlation");
    pending.delete(key);
    const body = JSON.parse(raw.toString("utf8"));
    const reply: ProviderReply = identity.purpose === "prediction" ? { text: "Continue the validated workflow" } : script(identity, body);
    if (reply.error) {
      response.writeHead(400, { "content-type": "application/json" });
      response.end(JSON.stringify({ type: "error", error: { type: "invalid_request_error", message: reply.error } }));
    } else if (body.stream === false) {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ id: `compact-${identity.execution}`, type: "message", role: "assistant", model: "fixture-model",
        content: [{ type: "text", text: reply.text }], stop_reason: "end_turn", stop_sequence: null, usage: { input_tokens: 40, output_tokens: 20 } }));
    } else {
      response.writeHead(200, { "content-type": "text/event-stream" }); response.end(wire(identity, reply));
    }
  }
  await new Promise<void>((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("cross-domain provider has no address");
  return {
    url: `http://127.0.0.1:${address.port}`, errors,
    correlate(identity: CallIdentity, body: Buffer) {
      const key = hash(body);
      if (pending.has(key)) throw new Error("ambiguous concurrent provider request body");
      pending.set(key, structuredClone(identity));
    },
    async close() { server.closeAllConnections(); await new Promise<void>((resolve) => server.close(() => resolve())); },
  };
}

/** Manual compact is bound to the explicitly issued command, never a guessed main execution. */
export class CrossIdentities {
  readonly agents: AgentIdentities;
  private pendingCompact?: string;
  private readonly compactExecutions = new Map<string, string>();
  private readonly diagnosticPrefixes = new Map<string, string>();
  constructor(readonly context: ReplayScenarioContext, anchors: ChildAnchor[]) { this.agents = new AgentIdentities(context, anchors); }
  arm(action: string) { this.agents.arm(action); }
  armCompact(action: string) {
    if (this.pendingCompact) throw new Error("manual compact already armed");
    this.pendingCompact = action;
  }
  resolve = async (raw: CallIdentity, run: any) => {
    if (raw.purpose !== "compact") return this.agents.resolve(raw, run);
    if (raw.owner !== this.context.sessionIds.root || raw.call !== "compact-command:0") throw new Error("unexpected compact execution identity");
    let action = this.compactExecutions.get(raw.execution);
    if (!action) {
      if (!this.pendingCompact) throw new Error("manual compact without explicit command");
      action = this.pendingCompact; this.pendingCompact = undefined; this.compactExecutions.set(raw.execution, action);
    }
    run.bindIdentity({ field: "owner", live: raw.owner, canonical: "root", anchor: "ACP session/new root" });
    run.bindIdentity({ field: "execution", owner: "root", live: raw.execution, canonical: action, anchor: `explicit manual compact ${action}` });
  };
  onRequest = async (observation: any) => {
    await this.agents.onRequest(observation);
    const body = JSON.parse(observation.request.body.toString("utf8"));
    for (const message of body.messages ?? []) for (const block of Array.isArray(message.content) ? message.content : []) {
      if (block.type !== "text" || typeof block.text !== "string") continue;
      const match = block.text.match(/^<system-reminder version="1" category="task" source="subagent" kind="failed"[^>]*metadata="([^"]*)">/u);
      if (!match) continue;
      const metadata = JSON.parse(match[1]!.replaceAll("&quot;", '"').replaceAll("&amp;", "&").replaceAll("&lt;", "<").replaceAll("&gt;", ">"));
      const child = metadata.subagent_failure?.child_thread_id;
      const registered = [...this.agents.children].find(([, id]) => id === child);
      if (!registered || metadata.child_thread_id !== child || metadata.success !== false) throw new Error("failure diagnostic lacks a registered failed child");
      const prefix = diagnosticOwnerPrefix(block.text, child);
      if (!prefix) continue;
      const alias = `child-${registered[0]}-diagnostic-owner-prefix-${prefix.length}`;
      const previous = this.diagnosticPrefixes.get(alias);
      if (previous && previous !== prefix) throw new Error("child diagnostic prefix changed within one run");
      if (!previous) {
        this.context.bindings.declareAndBind({ owner: "world", kind: "thread", alias, live: prefix,
          anchor: `resource_owner prefix in bounded failed-child diagnostic for ${registered[0]}` });
        this.diagnosticPrefixes.set(alias, prefix);
      }
    }
  };
}

/** The diagnostic is bounded by the product; only its confirmed child-id prefix may vary. */
export function diagnosticOwnerPrefix(text: string, child: string): string | undefined {
  const matches = [...text.matchAll(/ resource_owner: ([0-9a-f-]+)…\[truncated\](?=\nchild_thread_id: )/gu)];
  if (matches.length === 0) return undefined;
  if (matches.length !== 1) throw new Error("ambiguous truncated resource_owner diagnostic");
  const prefix = matches[0]![1]!;
  if (prefix.length < 8 || prefix.length >= child.length || !child.startsWith(prefix)) throw new Error("truncated diagnostic owner differs from actual child identity");
  return prefix;
}

export const key = (owner: string, execution: string, call: string) => ({ owner, execution, call, purpose: "reason", attempt: 1 });
export const bodyText = (observation: any) => JSON.stringify(JSON.parse(observation.request.body.toString("utf8")).messages);
export const occurrences = (text: string, marker: string) => text.split(marker).length - 1;

export function rootRows(context: ReplayScenarioContext): Array<{ message_id: string; excluded: number; content: string; role: string }> {
  const db = new DatabaseSync(replayStorePath(context.world), { readOnly: true });
  try {
    return db.prepare("SELECT message_id, excluded, content, role FROM messages WHERE thread_id = ? ORDER BY rowid")
      .all(context.sessionIds.root) as any;
  } finally { db.close(); }
}

export function uniqueResult(context: ReplayScenarioContext, marker: string) {
  const rows = rootRows(context).filter((row) => row.content.includes(marker));
  expect(rows).toHaveLength(1);
  return rows[0]!;
}

export function observedRequest(context: ReplayScenarioContext, execution: string, call: string, purpose = "reason") {
  const matches = context.requests.filter((entry) => entry.context.owner === "root" && entry.context.execution === execution
    && entry.context.call === call && entry.context.purpose === purpose);
  expect(matches).toHaveLength(1);
  return matches[0]!;
}
