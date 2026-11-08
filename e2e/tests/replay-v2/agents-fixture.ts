import { createHash } from "node:crypto";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { expect } from "vitest";
import type { ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";

export interface CallIdentity {
  owner: string; execution: string; call: string; purpose: string; attempt: number;
  parent?: { owner: string; execution: string; call: string; tool_call: string; slot?: string };
}
export interface ToolUse { id: string; name: string; input: Record<string, unknown> }
export interface Reply { text?: string; tools?: ToolUse[] }
export interface ChildAnchor {
  tool: string; alias: string; action?: string; parentCall?: string; execution?: string; background?: boolean;
}

export function text(reply: string): Reply { return { text: reply }; }
export function tools(...calls: ToolUse[]): Reply { return { tools: calls }; }
export function tool(id: string, name: string, input: Record<string, unknown>): ToolUse { return { id, name, input }; }
export function delegate(id: string, prompt: string, background = false): ToolUse {
  return tool(id, "Agent", { subagent_type: "replay-worker", prompt, run_in_background: background });
}
export function write(id: string, file: string, content: string): ToolUse {
  return tool(id, "Write", { file_path: file, content });
}
export function read(id: string, file: string): ToolUse { return tool(id, "Read", { file_path: file }); }

function event(type: string, data: object): string { return `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`; }
function encodeReply(identity: CallIdentity, reply: Reply): string {
  let result = event("message_start", { type: "message_start", message: {
    id: [identity.owner, identity.execution, identity.call].join("-"), type: "message", role: "assistant", model: "fixture-model",
    content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 20, output_tokens: 0 },
  } });
  let index = 0;
  if (reply.text !== undefined) {
    result += event("content_block_start", { type: "content_block_start", index, content_block: { type: "text", text: "" } });
    result += event("content_block_delta", { type: "content_block_delta", index, delta: { type: "text_delta", text: reply.text } });
    result += event("content_block_stop", { type: "content_block_stop", index });
    index++;
  }
  for (const use of reply.tools ?? []) {
    result += event("content_block_start", { type: "content_block_start", index, content_block: { type: "tool_use", id: use.id, name: use.name, input: {} } });
    result += event("content_block_delta", { type: "content_block_delta", index, delta: { type: "input_json_delta", partial_json: JSON.stringify(use.input) } });
    result += event("content_block_stop", { type: "content_block_stop", index });
    index++;
  }
  return result + event("message_delta", { type: "message_delta", delta: { stop_reason: reply.tools?.length ? "tool_use" : "end_turn", stop_sequence: null }, usage: { output_tokens: 10 } })
    + event("message_stop", { type: "message_stop" });
}

const digest = (body: Buffer) => createHash("sha256").update(body).digest("hex");

/** Context is correlated locally by the exact request bytes; the HTTP upstream receives no diagnostic headers. */
export async function agentProvider(script: (identity: CallIdentity, body: any) => Reply) {
  const pending = new Map<string, CallIdentity>();
  const errors: string[] = [];
  const requests: Array<{ identity: CallIdentity; body: any }> = [];
  const server = createServer((request, response) => { void handle(request, response).catch((error) => {
    errors.push(String(error));
    if (!response.headersSent) response.writeHead(500);
    response.end(String(error));
  }); });
  async function handle(request: IncomingMessage, response: ServerResponse) {
    const parts: Buffer[] = [];
    for await (const part of request) parts.push(Buffer.from(part));
    const raw = Buffer.concat(parts);
    if (request.headers["x-peri-replay-context"] || request.headers["x-peri-replay-target"]) throw new Error("proxy leaked routing metadata upstream");
    const identity = pending.get(digest(raw));
    if (!identity) throw new Error("upstream request has no exact local diagnostic correlation");
    pending.delete(digest(raw));
    const body = JSON.parse(raw.toString("utf8"));
    requests.push({ identity, body });
    const reply = identity.purpose === "prediction" ? text("Review the generated report") : script(identity, body);
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.end(encodeReply(identity, reply));
  }
  await new Promise<void>((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("fixture provider has no address");
  return {
    url: `http://127.0.0.1:${address.port}`, requests, errors,
    correlate(identity: CallIdentity, body: Buffer) {
      const key = digest(body);
      if (pending.has(key)) throw new Error("ambiguous concurrent upstream bodies cannot be correlated by arrival order");
      pending.set(key, structuredClone(identity));
    },
    async close() { server.closeAllConnections(); await new Promise<void>((resolve) => server.close(() => resolve())); },
  };
}

export async function configureAgent(world: { work: string }) {
  const directory = path.join(world.work, ".claude", "agents");
  await mkdir(directory, { recursive: true });
  await writeFile(path.join(directory, "replay-worker.md"), "---\nname: replay-worker\ndescription: A worker for deterministic delegation tests\nmodel: inherit\n---\n\nComplete the delegated task with the available workspace tools.\n");
}

export function contentText(value: any): string {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value.map(contentText).join("\n");
  if (value && typeof value === "object") return typeof value.text === "string" ? value.text : contentText(value.content);
  return "";
}
export function toolResult(body: any, id: string): any {
  const values = (body.messages ?? []).flatMap((message: any) => Array.isArray(message.content) ? message.content : []);
  const result = values.find((block: any) => block.type === "tool_result" && block.tool_use_id === id);
  if (!result) throw new Error(`real tool_result missing: ${id}`);
  return result;
}
export function resultText(body: any, id: string): string { return contentText(toolResult(body, id).content); }
export function expectResult(body: any, id: string, text: string, isError = false) {
  const result = toolResult(body, id);
  expect(Boolean(result.is_error)).toBe(isError);
  expect(contentText(result.content)).toContain(text);
}
export function childIdFromResult(body: any, toolId: string): string {
  const id = resultText(body, toolId).match(/(?:^|\n)child_thread_id: ([0-9a-f-]{36})(?:\s|$)/u)?.[1];
  if (!id) throw new Error(`Agent receipt lacks child_thread_id: ${toolId}`);
  return id;
}

/** Every child alias has a declared parent tool anchor. Arrival order never assigns actors. */
export class AgentIdentities {
  private pendingRoot?: string;
  private readonly rootExecutions = new Map<string, string>();
  private readonly childExecutions = new Map<string, string>();
  private readonly tokens = new Map<string, string>();
  readonly children = new Map<string, string>();
  readonly seen: CallIdentity[] = [];
  constructor(private readonly context: ReplayScenarioContext, readonly anchors: readonly ChildAnchor[]) {}
  arm(action: string) {
    if (this.pendingRoot) throw new Error("a root action is already armed");
    this.pendingRoot = action;
  }
  private bindToken(kind: "thread" | "task", alias: string, live: string, anchor: string) {
    const key = `${kind}:${alias}`, previous = this.tokens.get(key);
    if (previous && previous !== live) throw new Error(`token alias changed: ${key}`);
    if (previous) return;
    this.context.bindings.declareAndBind({ owner: "world", kind, alias, live, anchor });
    this.tokens.set(key, live);
  }
  private registerChild(alias: string, live: string) {
    const previous = this.children.get(alias);
    if (previous && previous !== live) throw new Error(`two child threads claimed ${alias}`);
    if ([...this.children].some(([other, id]) => other !== alias && id === live)) throw new Error("one child claimed two task slots");
    if (live === this.context.sessionIds.root) throw new Error("child incorrectly uses root owner");
    this.children.set(alias, live);
    this.bindToken("thread", `child-${alias}`, live, `created child ${alias}`);
  }
  resolve = async (raw: CallIdentity, run: any) => {
    const root = this.context.sessionIds.root;
    if (raw.owner === root) {
      run.bindIdentity({ field: "owner", live: root, canonical: "root", anchor: "ACP session/new root" });
      let execution = this.rootExecutions.get(raw.execution);
      if (!execution) {
        if (!this.pendingRoot || raw.purpose !== "reason" || raw.call !== "reason:1" || raw.parent) throw new Error(`unexpected root execution: ${JSON.stringify(raw)}`);
        execution = this.pendingRoot; this.pendingRoot = undefined;
        this.rootExecutions.set(raw.execution, execution);
      }
      run.bindIdentity({ field: "execution", owner: "root", live: raw.execution, canonical: execution, anchor: `explicit action ${execution}` });
    } else {
      const parent = raw.parent;
      const spec = this.anchors.find((entry) => entry.tool === parent?.tool_call);
      if (!parent || !spec || parent.owner !== root || this.rootExecutions.get(parent.execution) !== (spec.action ?? "turn-1")
        || parent.call !== (spec.parentCall ?? "reason:1") || parent.slot !== undefined) throw new Error(`unregistered child cause: ${JSON.stringify(raw)}`);
      this.registerChild(spec.alias, raw.owner);
      const execution = spec.execution ?? "created";
      const executionKey = JSON.stringify([raw.owner, raw.execution]);
      const prior = this.childExecutions.get(executionKey);
      if (prior && prior !== execution) throw new Error("child execution changed its causal anchor");
      this.childExecutions.set(executionKey, execution);
      run.bindIdentity({ field: "owner", live: raw.owner, canonical: spec.alias, anchor: `created child ${spec.alias}` });
      run.bindIdentity({ field: "execution", owner: spec.alias, live: raw.execution, canonical: execution, anchor: `Agent tool ${spec.tool}` });
    }
    this.seen.push(structuredClone(raw));
  };
  onRequest = async ({ context, request }: { context: CallIdentity; request: { body: Buffer } }) => {
    const body = JSON.parse(request.body.toString("utf8"));
    for (const anchor of this.anchors) {
      const result = (body.messages ?? []).flatMap((message: any) => Array.isArray(message.content) ? message.content : [])
        .find((block: any) => block.type === "tool_result" && block.tool_use_id === anchor.tool);
      if (!result) continue;
      const receipt = contentText(result.content);
      const child = receipt.match(/(?:^|\n)child_thread_id: ([0-9a-f-]{36})(?:\s|$)/u)?.[1];
      if (child) this.registerChild(anchor.alias, child);
      const task = receipt.match(/Background task (bg-[0-9a-f-]{36}) started\./u)?.[1];
      if (task) this.bindToken("task", `task-${anchor.tool}`, task, `Agent background receipt ${anchor.tool}`);
    }
    if (this.context.mode === "record") this.context.upstream!.correlate(context, request.body);
  };
}

export function identity(owner: string, call: string, execution = "created"): CallIdentity {
  return { owner, execution, call, purpose: "reason", attempt: 1 };
}
export function healthy(context: ReplayScenarioContext, predicate: (screen: string) => boolean) {
  return (screen: string) => {
    const errors = context.proxy.snapshot(context.runId).errors;
    if (errors.length) throw new Error(JSON.stringify(errors));
    if (context.upstream?.errors.length) throw new Error(context.upstream.errors.join("\n"));
    return predicate(screen);
  };
}
export async function completed(context: ReplayScenarioContext, tui: Awaited<ReturnType<ReplayScenarioContext["startTui"]>>, answer: string): Promise<string> {
  return tui.waitFor(healthy(context, (screen) => screen.includes(answer) && screen.includes("Brewed for")), `completed root answer ${answer}`, 45_000);
}
export async function assertFile(context: ReplayScenarioContext, name: string, expected: string) {
  expect(await readFile(path.join(context.world.work, name), "utf8")).toBe(expected);
}
export async function assertMissingFile(context: ReplayScenarioContext, name: string) {
  await expect(readFile(path.join(context.world.work, name), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
}

/** Read-only business persistence is independent of proxy attribution and fixture replies. */
export function persistedTrace(context: ReplayScenarioContext) {
  const database = new DatabaseSync(path.join(context.world.home, ".peri", "threads", "threads.db"), { readOnly: true });
  try {
    const root = context.sessionIds.root as string;
    const children = database.prepare("SELECT id, parent_thread_id FROM threads WHERE parent_thread_id = ? ORDER BY id").all(root) as Array<{ id: string; parent_thread_id: string }>;
    const ids = [root, ...children.map((child) => child.id)];
    const messages = database.prepare(`SELECT thread_id, role, content FROM messages WHERE thread_id IN (${ids.map(() => "?").join(",")}) ORDER BY rowid`).all(...ids)
      .map((row: any) => { const payload = JSON.parse(row.content); return { thread: row.thread_id as string, message: payload.type === "message" ? payload.message : payload }; });
    return { root, children, messages };
  } finally { database.close(); }
}
export function assertChildTrace(context: ReplayScenarioContext, actors: AgentIdentities, alias: string, spawnTool: string, file: string, report: string) {
  const trace = persistedTrace(context), child = actors.children.get(alias);
  expect(child).toBeDefined();
  expect(trace.children.some((entry) => entry.id === child && entry.parent_thread_id === trace.root)).toBe(true);
  const own = trace.messages.filter((entry) => entry.thread === child).map((entry) => entry.message);
  const writeCalls = own.flatMap((message: any) => message.tool_calls ?? []).filter((call: any) => call.name === "Write");
  expect(writeCalls.some((call: any) => (call.arguments ?? call.input)?.file_path === file)).toBe(true);
  expect(own.some((message: any) => message.role === "assistant" && contentText(message.content).includes(report))).toBe(true);
  const receipts = trace.messages.filter((entry) => entry.thread === trace.root)
    .map((entry) => entry.message).filter((message: any) => message.role === "tool" && message.tool_call_id === spawnTool);
  expect(receipts).toHaveLength(1);
  expect(contentText(receipts[0].content)).toContain(child!);
  return { childTools: writeCalls.map((call: any) => ({ name: call.name, file: (call.arguments ?? call.input)?.file_path })), reportPersisted: true };
}
