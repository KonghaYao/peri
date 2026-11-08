import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { expect } from "vitest";
import type { ReplayScenarioContext, ReplayRequestObservation } from "../../helpers/llm-replay-suite.js";
import type { ReplayTui, ReplayWorld } from "../../helpers/llm-replay-tui.js";
import { contentText, resultText, tool, type CallIdentity } from "../replay-v2/agents-fixture.js";

export const monitorNames = ["monitor_start", "monitor_read", "monitor_stop", "monitor_list"];
export const portableMonitorTools = { ExecuteExtraTool: ["/params/task_id"] };
export const discoverMonitor = (id: string) => tool(`${id}-discover`, "SearchExtraTools", {
  query: `select:${monitorNames.map((name) => `mcp__monitor__${name}`).join(",")}`,
});
export const monitorTool = (id: string, name: string, params: Record<string, unknown>) =>
  tool(id, "ExecuteExtraTool", { tool_name: `mcp__monitor__${name}`, params });
export const monitorCall = (execution: string, call: string, owner = "root"): CallIdentity =>
  ({ owner, execution, call, purpose: "reason", attempt: 1 });
export const monitorCommand = (name: string, mode = name) => `python3 monitor-source.py ${name} ${mode}`;

// This is the external command under test. All capture, buffering, Tasks polling,
// owner routing, cancellation and TUI projection remain real Peri code.
const source = String.raw`import json, os, pathlib, subprocess, sys, time
name, mode = sys.argv[1:3]
def mark(suffix, value):
    pathlib.Path(name + suffix).write_text(value)
def gate(suffix):
    end = time.monotonic() + 90
    while not pathlib.Path(name + suffix).exists():
        if time.monotonic() > end: raise RuntimeError('source gate timed out: ' + suffix)
        time.sleep(.005)
def emit(text, stream='stdout', newline=True):
    raw = text + ('\n' if newline else '')
    with open(name + '.ledger', 'a') as ledger: ledger.write(json.dumps({'text':text,'stream':stream,'newline':newline}, ensure_ascii=False) + '\n')
    target = sys.stdout if stream == 'stdout' else sys.stderr
    target.write(raw); target.flush()
mark('.pid', str(os.getpid()))
with open(name + '.starts', 'a') as starts: starts.write('started\n')
if mode == 'tree':
    child = subprocess.Popen([sys.executable, '-c', "import os,pathlib,time; pathlib.Path('" + name + ".child').write_text(str(os.getpid())); time.sleep(90)"])
    while not pathlib.Path(name + '.child').exists(): time.sleep(.005)
    emit(name.upper() + '_TREE_READY')
    mark('.flushed', 'stdout flushed')
    gate('.never')
elif mode == 'deadline':
    emit(name.upper() + '_DEADLINE_READY')
    gate('.never')
elif mode in ('incremental', 'alerts'):
    gate('.release')
    emit(name.upper() + '_ALERT_1')
    mark('.first', 'emitted')
    gate('.second')
    emit(name.upper() + '_ALERT_2')
    emit(name.upper() + '_ALERT_3')
    mark('.second-done', 'emitted')
    gate('.finish')
else:
    gate('.release')
    if mode == 'streams':
        emit(name.upper() + '_STDOUT')
        emit(name.upper() + '_PARTIAL', newline=False)
    elif mode == 'pages':
        for i in range(1, 8): emit(name.upper() + '_SEQ_' + str(i))
    elif mode == 'overflow':
        for i in range(1, 11): emit(name.upper() + '_SEQ_' + str(i))
    elif mode == 'unicode':
        emit('文🧪' * 5000)
        emit(name.upper() + '_AFTER_LONG')
    elif mode == 'failure':
        emit(name.upper() + '_FAILURE', 'stderr')
        sys.exit(7)
    else: emit(name.upper() + '_ACTUAL_EVENT')
mark('.ended', 'source completed')
`;

export async function configureMonitor(world: ReplayWorld): Promise<void> {
  await mkdir(path.join(world.home, "tmp"), { recursive: true });
  await writeFile(path.join(world.work, "monitor-source.py"), source);
}
export async function startMonitorTui(ctx: ReplayScenarioContext, alias = "root") {
  return ctx.startTui(alias, { rows: 52, cols: 155, env: {
    TMPDIR: path.join(ctx.world.home, "tmp"), TMP: path.join(ctx.world.home, "tmp"), TEMP: path.join(ctx.world.home, "tmp"),
  } });
}
export async function monitorGate(ctx: ReplayScenarioContext, name: string, suffix = ".release") {
  await writeFile(path.join(ctx.world.work, name + suffix), "release\n");
}
export async function closeMonitorPanel(ctx: ReplayScenarioContext, tui: ReplayTui) {
  await tui.key("Escape");
  await tui.waitFor(ctx.checked((screen: string) => !screen.includes("─ Tasks ─")), "Tasks panel is actually closed before further input", 10_000);
}
export async function monitorFile(ctx: ReplayScenarioContext, file: string): Promise<string> {
  await expect.poll(async () => {
    expect(ctx.proxy.snapshot(ctx.runId).errors).toEqual([]);
    try { return (await readFile(path.join(ctx.world.work, file), "utf8")).length > 0; }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; return false; }
  }, { timeout: 25_000, interval: 20 }).toBe(true);
  return readFile(path.join(ctx.world.work, file), "utf8");
}
export async function noMonitorFile(ctx: ReplayScenarioContext, file: string) {
  await expect(access(path.join(ctx.world.work, file))).rejects.toMatchObject({ code: "ENOENT" });
}
export async function monitorDone(ctx: ReplayScenarioContext, tui: ReplayTui, marker: string) {
  const screen = await tui.waitFor(ctx.checked((screen: string) => {
    if (ctx.upstream?.errors.length) throw new Error(ctx.upstream.errors.join("\n"));
    return screen.includes(marker) && screen.includes("Brewed for");
  }), marker, 45_000);
  await ctx.waitForCalls((state) => state.active === 0);
  return screen;
}
function monitorJson(text: string): { value: any; json: string } {
  const suffix = "\n[Execution status: completed]";
  expect(text.endsWith(suffix)).toBe(true);
  const json = text.slice(0, -suffix.length); return { value: JSON.parse(json), json };
}
export function jsonResult(body: any, id: string): any { return monitorJson(resultText(body, id)).value; }
export function bodyText(body: any): string { return (body.messages ?? []).map((message: any) => contentText(message.content)).join("\n"); }
export function requestBody(ctx: ReplayScenarioContext, execution: string, call: string, owner = "root") {
  const requests = ctx.requests.filter((item) => item.context.owner === owner && item.context.execution === execution && item.context.call === call);
  expect(requests, `exactly one monitor request: ${owner}/${execution}/${call}`).toHaveLength(1);
  return JSON.parse(requests[0]!.request.body.toString());
}
export function assertPage(page: any, after: number) {
  expect(Array.isArray(page.events)).toBe(true);
  const sequence = page.events.map((event: any) => event.seq);
  expect(sequence.every((seq: number, i: number) => Number.isSafeInteger(seq) && seq > (i ? sequence[i - 1]! : after))).toBe(true);
  expect(page.nextSeq).toBe(sequence.at(-1) ?? after);
  expect(["working", "completed", "failed", "cancelled"]).toContain(page.status);
  expect(Number.isSafeInteger(page.dropped) && page.dropped >= 0).toBe(true);
  for (const event of page.events) {
    expect(event.untrusted).toBe(true); expect(event.kind).toBe("line");
    expect(["stdout", "stderr"]).toContain(event.stream);
    expect(event.bytes).toBeGreaterThanOrEqual(Buffer.byteLength(event.text));
    expect(event.text).not.toContain("\ufffd");
  }
}

interface JsonString { path: string; value: string; start: number; end: number }
/** Track exact JSON string spans, including nesting, so body text that resembles a date stays literal. */
function jsonStrings(text: string): JsonString[] {
  const spans: JsonString[] = []; let at = 0;
  const space = () => { while (/\s/u.test(text[at] ?? "") && at < text.length) at++; };
  const string = () => {
    const start = at++; while (at < text.length) { if (text[at] === "\\") at += 2; else if (text[at++] === '"') break; }
    return { value: JSON.parse(text.slice(start, at)) as string, start: start + 1, end: at - 1 };
  };
  const visit = (pointer: string) => {
    space();
    if (text[at] === '"') { spans.push({ path: pointer, ...string() }); return; }
    if (text[at] === "{") {
      at++; space(); if (text[at] === "}") { at++; return; }
      while (at < text.length) { space(); const key = string().value; space(); expect(text[at++]).toBe(":");
        visit(`${pointer}/${key.replace(/~/gu, "~0").replace(/\//gu, "~1")}`); space(); const delimiter = text[at++];
        if (delimiter === "}") return; expect(delimiter).toBe(","); }
    } else if (text[at] === "[") {
      at++; space(); if (text[at] === "]") { at++; return; }
      for (let index = 0; at < text.length; index++) { visit(`${pointer}/${index}`); space(); const delimiter = text[at++];
        if (delimiter === "]") return; expect(delimiter).toBe(","); }
    } else { while (at < text.length && !/[\s,\]}]/u.test(text[at]!)) at++; }
  };
  JSON.parse(text); visit(""); space(); expect(at).toBe(text.length); return spans;
}

export interface MonitorSpec { id: string; alias: string; command: string; owner?: string }
interface EventFact { seq: number; ts: string; stream: string; [key: string]: any }
export class MonitorBindings {
  readonly tasks = new Map<string, { raw: string; host: string }>();
  readonly results = new Map<string, any>();
  readonly events = new Map<string, Map<number, EventFact>>();
  private readonly values = new Map<string, string>();
  private readonly rules = new Map<string, any[]>();
  private readonly started = Date.now();
  constructor(readonly ctx: ReplayScenarioContext, readonly specs: MonitorSpec[]) {}
  raw(alias: string) { const id = this.tasks.get(alias)?.raw; if (!id) throw new Error(`monitor receipt missing ${alias}`); return id; }
  host(alias: string) { const id = this.tasks.get(alias)?.host; if (!id) throw new Error(`host receipt missing ${alias}`); return id; }
  private bind(kind: string, alias: string, live: string, anchor: string) {
    const key = `${kind}:${alias}`, prior = this.values.get(key);
    if (prior !== undefined) { expect(live).toBe(prior); return; }
    this.ctx.bind({ kind, alias, live, anchor }); this.values.set(key, live);
  }
  private timestamp(value: string) {
    expect(value).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/u);
    const ms = Date.parse(value); expect(Number.isFinite(ms)).toBe(true);
    expect(ms).toBeGreaterThanOrEqual(this.started - 5_000); expect(ms).toBeLessThanOrEqual(Date.now() + 1_000);
    return ms;
  }
  private inspectSnapshot(value: any) {
    const start = this.timestamp(value.startedAt), updated = this.timestamp(value.lastUpdatedAt), deadline = Date.parse(value.deadlineAt);
    expect(Number.isFinite(deadline)).toBe(true); expect(updated).toBeGreaterThanOrEqual(start); expect(deadline).toBeGreaterThan(start);
    expect(deadline - start).toBeLessThanOrEqual(86_400_001);
    if (value.finishedAt !== null) { expect(this.timestamp(value.finishedAt)).toBeGreaterThanOrEqual(start); expect(Date.parse(value.finishedAt)).toBeLessThanOrEqual(updated); }
  }
  private inspectResult(id: string, name: string, params: any, value: any) {
    if (name === "monitor_read") {
      const spec = this.specs.find((spec) => this.tasks.get(spec.alias)?.raw === params.task_id);
      expect(spec).toBeDefined(); assertPage(value, params.after_seq ?? 0);
      const known = this.events.get(spec!.alias) ?? new Map<number, EventFact>();
      for (const event of value.events) {
        this.timestamp(event.ts); const prior = known.get(event.seq); if (prior) expect(event).toEqual(prior); else known.set(event.seq, event);
      }
      for (const stream of ["stdout", "stderr"]) {
        const ordered = [...known.values()].filter((event) => event.stream === stream).sort((a, b) => a.seq - b.seq);
        for (let i = 1; i < ordered.length; i++) expect(Date.parse(ordered[i]!.ts)).toBeGreaterThanOrEqual(Date.parse(ordered[i - 1]!.ts));
      }
      this.events.set(spec!.alias, known);
    } else if (name === "monitor_stop") { expect(value.taskId).toBe(params.task_id); this.inspectSnapshot(value); }
    else if (name === "monitor_list") { expect(Array.isArray(value.tasks)).toBe(true); value.tasks.forEach((item: any) => this.inspectSnapshot(item)); }
    this.results.set(id, value);
  }
  private template(id: string, name: string, params: any, value: any, text: string, pointer: string) {
    const spans = jsonStrings(monitorJson(text).json), replacements: Array<JsonString & { kind: string; alias: string }> = [];
    const ownerSpec = this.specs.find((spec) => this.tasks.get(spec.alias)?.raw === params.task_id);
    for (const span of spans) {
      if (/^\/events\/\d+\/ts$/u.test(span.path) && name === "monitor_read") {
        const index = Number(span.path.split("/")[2]), seq = value.events[index].seq;
        const alias = `monitor-${ownerSpec!.alias}-event-${seq}-ts`;
        this.bind("timestamp", alias, span.value, `monitor ${ownerSpec!.id} event seq ${seq} capture timestamp`);
        replacements.push({ ...span, kind: "timestamp", alias });
      } else if (/\/(?:startedAt|lastUpdatedAt|deadlineAt|finishedAt)$/u.test(span.path) && ["monitor_stop", "monitor_list"].includes(name)) {
        const task = name === "monitor_list" ? value.tasks[Number(span.path.split("/")[2])] : value;
        const spec = this.specs.find((candidate) => this.tasks.get(candidate.alias)?.raw === task.taskId);
        expect(spec).toBeDefined();
        const alias = `monitor-${spec!.alias}-${id}-${span.path.split("/").at(-1)}`;
        this.bind("timestamp", alias, span.value, `monitor ${spec!.id} tool ${id} field ${span.path}`);
        replacements.push({ ...span, kind: "timestamp", alias });
      } else if (span.path.endsWith("/taskId")) {
        const spec = this.specs.find((candidate) => this.tasks.get(candidate.alias)?.raw === span.value);
        expect(spec).toBeDefined(); replacements.push({ ...span, kind: "task", alias: `monitor-${spec!.alias}-raw` });
      }
    }
    if (!replacements.length) return undefined;
    let cursor = 0; const segments: any[] = [];
    for (const replacement of replacements) {
      expect(text.slice(replacement.start, replacement.end)).toBe(replacement.value);
      segments.push(text.slice(cursor, replacement.start), { kind: replacement.kind, alias: replacement.alias }); cursor = replacement.end;
    }
    segments.push(text.slice(cursor)); return { path: pointer, mode: "typed-template", segments };
  }
  onRequest = async (observation: ReplayRequestObservation) => {
    const body = JSON.parse(observation.request.body.toString()), calls = new Map<string, any>(), rules: any[] = [];
    for (const message of body.messages ?? []) for (const block of Array.isArray(message.content) ? message.content : []) {
      if (block.type === "tool_use" && block.name === "ExecuteExtraTool" && block.input?.tool_name?.startsWith("mcp__monitor__")) calls.set(block.id, block.input);
    }
    for (let mi = 0; mi < (body.messages ?? []).length; mi++) {
      const blocks = body.messages[mi].content; if (!Array.isArray(blocks)) continue;
      for (let bi = 0; bi < blocks.length; bi++) {
        const block = blocks[bi], input = calls.get(block.tool_use_id);
        if (block.type !== "tool_result" || !input) continue;
        const id = block.tool_use_id, name = input.tool_name.slice("mcp__monitor__".length), text = contentText(block.content);
        if (name === "monitor_start" && !block.is_error) {
          const spec = this.specs.find((candidate) => candidate.id === id); expect(spec).toBeDefined(); expect(input.params.command).toBe(spec!.command);
          const host = text.match(/Background task started: (mcp-[A-Za-z0-9_-]+)\./u)?.[1];
          const raw = text.match(/\)\. monitor_id: (monitor-[A-Za-z0-9_-]+) \(use this/u)?.[1];
          expect(host).toBeTruthy(); expect(raw).toBeTruthy(); expect(host).not.toBe(raw);
          this.bind("task", `monitor-${spec!.alias}-host`, host!, `monitor_start host receipt ${id}`);
          this.bind("task", `monitor-${spec!.alias}-raw`, raw!, `monitor_start raw receipt ${id}`);
          this.tasks.set(spec!.alias, { host: host!, raw: raw! }); continue;
        }
        if (block.is_error || name === "monitor_start") continue;
        const value = monitorJson(text).value; this.inspectResult(id, name, input.params, value);
        const base = `/messages/${mi}/content/${bi}/content`;
        const parts = typeof block.content === "string" ? [{ text: block.content, pointer: base }] :
          block.content.map((part: any, index: number) => ({ text: part.text, pointer: `${base}/${index}/text` }));
        expect(parts).toHaveLength(1);
        const rule = this.template(id, name, input.params, value, parts[0]!.text, parts[0]!.pointer); if (rule) rules.push(rule);
      }
    }
    this.rules.set(JSON.stringify(observation.context), rules);
    if (this.ctx.mode === "record") this.ctx.upstream!.correlate(observation.context, observation.request.body);
  };
  requestRules = (call: CallIdentity) => this.rules.get(JSON.stringify(call)) ?? [];
}
