import { createServer } from "node:http";
import { readFile, writeFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import path from "node:path";
import { expect } from "vitest";
import type { ReplayScenarioContext } from "../../helpers/llm-replay-suite.js";
import { REPLAY_BINARY, replayEnvironment, type ReplayTui, type ReplayWorld } from "../../helpers/llm-replay-tui.js";
import { startStdioAcpFixture } from "../../helpers/stdio-acp-fixture.js";
import { replayStorePath } from "../../helpers/replay-isolation.js";

export interface ContextReply {
  text?: string;
  tail?: string;
  tool?: { id: string; name: string; input: object };
  status?: number;
  error?: string;
  stop?: string;
  inputTokens?: number;
}

const event = (type: string, data: object): string => `event: ${type}\ndata: ${JSON.stringify(data)}\n\n`;
export function textPrefix(text: string, inputTokens = 30): string {
  return event("message_start", { type: "message_start", message: { id: "context-message", type: "message", role: "assistant", model: "fixture-model", content: [], stop_reason: null, usage: { input_tokens: inputTokens, output_tokens: 0 } } })
    + event("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } })
    + event("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } });
}
function wire(reply: ContextReply): string {
  const tool = reply.tool;
  const start = tool
    ? event("message_start", { type: "message_start", message: { id: "context-tool", type: "message", role: "assistant", model: "fixture-model", content: [], stop_reason: null, usage: { input_tokens: reply.inputTokens ?? 30, output_tokens: 0 } } })
      + event("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "tool_use", id: tool.id, name: tool.name, input: {} } })
      + event("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: JSON.stringify(tool.input) } })
    : textPrefix(reply.text ?? "", reply.inputTokens);
  return start + (reply.tail ? event("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: reply.tail } }) : "")
    + event("content_block_stop", { type: "content_block_stop", index: 0 })
    + event("message_delta", { type: "message_delta", delta: { stop_reason: reply.stop ?? (tool ? "tool_use" : "end_turn") }, usage: { output_tokens: 20 } })
    + event("message_stop", { type: "message_stop" });
}

export async function contextModel(select: (body: any) => ContextReply) {
  const errors: string[] = [];
  const server = createServer(async (req, res) => {
    try {
      const chunks: Buffer[] = []; for await (const chunk of req) chunks.push(Buffer.from(chunk));
      const body = JSON.parse(Buffer.concat(chunks).toString());
      const reply = select(body);
      if (reply.status) {
        res.writeHead(reply.status, { "content-type": "application/json" });
        res.end(JSON.stringify({ type: "error", error: { type: "invalid_request_error", message: reply.error } }));
      } else if (body.stream === false) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ id: "context-complete", type: "message", role: "assistant", model: "fixture-model", content: [{ type: "text", text: reply.text }], stop_reason: reply.stop ?? "end_turn", stop_sequence: null, usage: { input_tokens: reply.inputTokens ?? 30, output_tokens: 20 } }));
      } else { res.writeHead(200, { "content-type": "text/event-stream" }); res.end(wire(reply)); }
    } catch (error) { errors.push(String(error)); res.destroy(error as Error); }
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address(); if (!address || typeof address === "string") throw new Error("context fixture address missing");
  return { url: `http://127.0.0.1:${address.port}`, errors,
    close: () => new Promise<void>((resolve) => { server.closeAllConnections(); server.close(() => resolve()); }) };
}

export const bodyOf = (entry: any): any => JSON.parse(entry.request.body.toString());
export const modelText = (body: any): string => JSON.stringify(body.messages ?? []);
export function userTexts(body: any): string[] {
  return (body.messages ?? []).filter((m: any) => m.role === "user").map((m: any) => typeof m.content === "string" ? m.content
    : (m.content ?? []).filter((b: any) => b.type === "text" && !b.text.trimStart().startsWith("<system-reminder ")).map((b: any) => b.text).join("\n")).filter(Boolean);
}
export const latestUser = (body: any): string => userTexts(body).at(-1) ?? "";
export const resultBlocks = (body: any): any[] => (body.messages ?? []).flatMap((m: any) => Array.isArray(m.content) ? m.content : []).filter((b: any) => b.type === "tool_result");
export function resultText(result: any): string {
  return typeof result?.content === "string" ? result.content : (result?.content ?? []).map((b: any) => b.text ?? "").join("\n");
}
export const gateKey = (execution: string) => ({ owner: "root", execution, call: "reason:1", attempt: 1, purpose: "reason" });
export const CTRL_HOME = "\x1b[1;5H", CTRL_END = "\x1b[1;5F", ALT_UP = "\x1b[1;3A", ALT_DOWN = "\x1b[1;3B";

export async function turn(ctx: ReplayScenarioContext, tui: ReplayTui, execution: string, input: string, output: string, calls = 1): Promise<string> {
  const total = ctx.requests.length + calls;
  ctx.identity.arm(ctx.sessionIds.root!, execution);
  await tui.prompt(input);
  const screen = await tui.waitFor(ctx.checked((s: string) => s.includes(output) && s.includes("Brewed for")), `${execution} terminal response`);
  await ctx.waitForCalls((s: any) => s.active === 0 && s.calls.length === total);
  ctx.identity.finish(ctx.sessionIds.root!);
  return screen;
}

export function stored(world: ReplayWorld, sessionId: string): Array<{ id: string; role: string; content: string; excluded: number; projection: string | null }> {
  const db = new DatabaseSync(replayStorePath(world), { readOnly: true });
  try { return db.prepare("SELECT message_id AS id, role, content, excluded, projection FROM messages WHERE thread_id = ? ORDER BY rowid").all(sessionId) as any; }
  finally { db.close(); }
}

/** Ctrl+Home must traverse actual application pages; xterm scrollback is never consulted. */
export async function seekOldest(tui: ReplayTui, marker: string): Promise<string> {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    await tui.type(CTRL_HOME);
    const screen = await tui.screen();
    if (screen.includes(marker)) return screen;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`oldest history marker unavailable: ${marker}\n${await tui.screen()}`);
}

export async function clipboardPayloads(tui: ReplayTui, name: string): Promise<string[]> {
  await tui.save(name);
  const raw = (await readFile(path.join(tui.world.evidence, `${tui.label}-${name}.pty`))).toString();
  return [...raw.matchAll(/\x1b\]52;c;([A-Za-z0-9+/=]*)(?:\x07|\x1b\\)/g)].map((m) => Buffer.from(m[1]!, "base64").toString("utf8"));
}

/** A fresh real stdio ACP process reads the same isolated store without running a model. */
export async function inspectWindow(ctx: ReplayScenarioContext, sessionId: string, pageLimit = 50) {
  const peer = await startStdioAcpFixture({ binary: REPLAY_BINARY, cwd: ctx.world.work, home: ctx.world.home, dbPath: replayStorePath(ctx.world), env: replayEnvironment(ctx.world),
    onNotification() {}, onServerRequest() {}, handleServerRequest(r) { throw new Error(`unexpected history reader request ${r.method}`); } });
  const pages: any[] = [], requests: any[] = [];
  let exitCode: number | null | undefined;
  try {
    const initialized = await peer.call("initialize", { protocolVersion: 1, clientCapabilities: { _meta: { "peri.historyWindowV1": true, "peri.systemReminder": true } } });
    expect(initialized.error).toBeUndefined();
    const loaded = await peer.call("session/load", { sessionId, cwd: ctx.world.work, mcpServers: [] });
    expect(loaded.error, JSON.stringify(loaded.error)).toBeUndefined();
    const ask = async (history: object) => {
      const params = { sessionId, history }; requests.push(params);
      const response = await peer.call("session/load", params); expect(response.error, JSON.stringify(response.error)).toBeUndefined();
      pages.push(response.result); return response.result;
    };
    let page = await ask({ kind: "open", direction: "before", limit: pageLimit });
    while (page.beforeCursor) {
      if (pages.length > 20) throw new Error("history reader failed to terminate");
      page = await ask({ kind: "page", snapshotToken: page.snapshotToken, readTicket: page.readTicket, beforeCursor: page.beforeCursor, limit: pageLimit });
    }
    exitCode = await peer.close(); expect(exitCode).toBe(0);
    return pages;
  } finally {
    if (exitCode === undefined) exitCode = await peer.close();
    await writeFile(path.join(ctx.world.evidence, "history-wire.json"), JSON.stringify({ requests, pages, exitCode, diagnostics: await peer.diagnostics() }, null, 2));
    await writeFile(path.join(ctx.world.evidence, "history-acp-process.json"), JSON.stringify({ exitCode, graceful: exitCode === 0 }, null, 2));
  }
}
