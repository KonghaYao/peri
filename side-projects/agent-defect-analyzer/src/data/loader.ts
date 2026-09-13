//! Read-only, schema-aware transcript access shared by analyzer commands.

import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";
import type { MessageRow, NormalizedMessage, SchemaCapabilities, ThreadRow, ThreadSummary, ToolCall, ToolExecutionEvidence, ToolExecutionStatus, ToolResult } from "./types.js";
export * from "./types.js";

export const DEFAULT_DB_PATH = join(homedir(), ".peri/threads/threads.db");
export const NORMALIZER_VERSION = "peri-normalizer-v3";
const REQUIRED: Record<string, string[]> = {
  threads: ["id", "title", "cwd", "created_at", "updated_at", "message_count"],
  messages: ["message_id", "thread_id", "role", "content"],
};
const OPTIONAL: Record<string, string[]> = {
  threads: ["parent_thread_id", "snapshot_at_message_id", "hidden", "cancel_policy", "config", "cached_context", "frozen_context", "inherited_context", "agent_status", "context_cache_epoch"],
  messages: ["truncated", "excluded", "projection"],
};

export class SchemaCompatibilityError extends Error {
  constructor(readonly table: string, readonly missing: string[]) {
    super(`incompatible session schema: ${table} missing ${missing.join(", ")}`);
    this.name = "SchemaCompatibilityError";
  }
}
export class InheritedContextError extends Error {
  constructor(readonly issue: string) { super(`invalid inherited context: ${issue}`); this.name = "InheritedContextError"; }
}

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj | null => v !== null && typeof v === "object" && !Array.isArray(v) ? v as Obj : null;
const str = (v: unknown): string | null => typeof v === "string" ? v : null;
const bool = (v: unknown): boolean | null => typeof v === "boolean" ? v : v === 1 ? true : v === 0 ? false : null;
const text = (v: unknown): string => typeof v === "string" ? v : Array.isArray(v) ? v.map((x) => typeof obj(x)?.text === "string" ? obj(x)!.text as string : "").join("") : "";
const EXECUTION_STATUSES: ReadonlySet<string> = new Set(["unknown", "completed", "failed", "cancelled", "timed_out", "running", "running_after_timeout"]);

function unknownExecution(source: "typed" | "legacy" = "legacy"): ToolExecutionEvidence {
  return { status: "unknown", exitCode: null, outputRef: null, outputTruncated: false, taskId: null, source };
}

function executionEqual(left: ToolExecutionEvidence, right: ToolExecutionEvidence): boolean {
  return left.status === right.status && left.exitCode === right.exitCode && left.outputRef === right.outputRef &&
    left.outputTruncated === right.outputTruncated && left.taskId === right.taskId;
}

/** Parse only the persisted typed `execution` object; result text is never a source of execution facts. */
function parseExecution(payload: Obj, issues: string[], isError: boolean | null, invalidIds: Set<string>, id: string): ToolExecutionEvidence {
  if (!("execution" in payload) || payload.execution === null) return unknownExecution();
  const raw = obj(payload.execution);
  if (!raw) { issues.push("invalidExecutionMetadata"); invalidIds.add(id); return unknownExecution("typed"); }
  const unknownFields = Object.keys(raw).filter((key) => !["status", "exit_code", "output_ref", "output_truncated", "task_id"].includes(key));
  for (const key of unknownFields) issues.push(`unknownExecutionField:${key}`);
  const status = str(raw.status);
  if (!status || !EXECUTION_STATUSES.has(status)) { issues.push("invalidExecutionStatus"); invalidIds.add(id); return unknownExecution("typed"); }
  let invalid = false;
  const exitCodeValue = raw.exit_code;
  let exitCode: number | null = null;
  if (exitCodeValue !== undefined && exitCodeValue !== null) {
    if (typeof exitCodeValue !== "number" || !Number.isInteger(exitCodeValue) || exitCodeValue < -2147483648 || exitCodeValue > 2147483647) { issues.push("invalidExecutionExitCode"); invalid = true; }
    else exitCode = exitCodeValue;
  }
  const outputRefValue = raw.output_ref;
  let outputRef: string | null = null;
  if (outputRefValue !== undefined && outputRefValue !== null) {
    if (typeof outputRefValue !== "string") { issues.push("invalidExecutionOutputRef"); invalid = true; }
    else outputRef = outputRefValue;
  }
  const truncatedValue = raw.output_truncated;
  let outputTruncated = false;
  if (truncatedValue !== undefined) {
    if (typeof truncatedValue !== "boolean") { issues.push("invalidExecutionOutputTruncated"); invalid = true; }
    else outputTruncated = truncatedValue;
  }
  const taskIdValue = raw.task_id;
  let taskId: string | null = null;
  if (taskIdValue !== undefined && taskIdValue !== null) {
    if (typeof taskIdValue !== "string") { issues.push("invalidExecutionTaskId"); invalid = true; }
    else taskId = taskIdValue;
  }
  // Rust's ToolResult::from_output marks `running` as is_error=false; the
  // terminal promotion state remains an error-like running_after_timeout.
  const errorStatus = ["failed", "cancelled", "timed_out", "running_after_timeout"].includes(status);
  if (isError !== null && status !== "unknown" && errorStatus !== isError) { issues.push("conflictingExecutionErrorFlag"); invalid = true; }
  // Current producers leave exit_code empty for non-terminal lifecycle states.
  if ((status === "completed" && exitCode !== null && exitCode !== 0) || (status === "failed" && exitCode === 0) ||
    (["cancelled", "timed_out", "running", "running_after_timeout"].includes(status) && exitCode !== null)) {
    issues.push("conflictingExecutionExitCode"); invalid = true;
  }
  if (invalid) { invalidIds.add(id); return unknownExecution("typed"); }
  return { status: status as ToolExecutionStatus, exitCode, outputRef, outputTruncated, taskId, source: "typed" };
}

function mergeExecution(existing: ToolExecutionEvidence, incoming: ToolExecutionEvidence, issues: string[], invalidIds: Set<string>, id: string): ToolExecutionEvidence {
  if (invalidIds.has(id)) return unknownExecution("typed");
  if (existing.source === "legacy" && incoming.source === "typed") return incoming;
  if (existing.source === "typed" && incoming.source === "legacy") return existing;
  if (!executionEqual(existing, incoming)) { issues.push(`conflictingExecutionMetadata:${id}`); invalidIds.add(id); return unknownExecution("typed"); }
  return existing;
}

function sameJson(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (typeof left !== typeof right || left === null || right === null) return false;
  if (Array.isArray(left) || Array.isArray(right)) {
    return Array.isArray(left) && Array.isArray(right) && left.length === right.length && left.every((v, i) => sameJson(v, right[i]));
  }
  if (typeof left !== "object") return false;
  const l = left as Obj, r = right as Obj;
  const lk = Object.keys(l), rk = Object.keys(r);
  return lk.length === rk.length && lk.every((key) => Object.prototype.hasOwnProperty.call(r, key) && sameJson(l[key], r[key]));
}

function addSource(sources: Array<"content" | "tool_calls" | "message">, source: "content" | "tool_calls" | "message"): void {
  if (!sources.includes(source)) sources.push(source);
}

/** Normalize a row while retaining parse diagnostics and unknown error state. */
export function normalizeMessage(row: MessageRow, origin: "own" | "inherited" = "own"): NormalizedMessage {
  const issues: string[] = [];
  let p: Obj | null = null;
  try {
    const value: unknown = JSON.parse(row.content);
    p = obj(value);
    if (p && ("version" in p || "type" in p)) {
      if (!("version" in p)) {
        issues.push("unsupportedEnvelope");
        p = null;
      } else if (p.version !== 1) {
        issues.push("unsupportedEnvelope");
        p = null;
      } else if (p.type === "message") {
        p = obj(p.message);
        if (!p) issues.push("message_envelope_missing_message");
      } else if (p.type === "system_reminder") {
        // Reminders are canonical payloads, not summaries or model messages.
        p = {};
      } else {
        issues.push("unsupportedEnvelope");
        p = null;
      }
    }
    if (!p && !issues.includes("unsupportedEnvelope") && !issues.includes("message_envelope_missing_message")) issues.push("payload_not_object");
    if (p && row.role !== "system_reminder" && !("role" in p || "content" in p)) issues.push("payload_missing_role_or_content");
  }
  catch { issues.push("invalidJson"); }
  const calls: ToolCall[] = [], results: ToolResult[] = [];
  const invalidExecutionIds = new Set<string>();
  const callIds = new Set<string>(), resultIds = new Set<string>();
  const blocks = Array.isArray(p?.content) ? p!.content : [];
  for (const raw of blocks) {
    const b = obj(raw); if (!b) { issues.push("content_block_not_object"); continue; }
    if (b.type === "tool_use") {
      const id = str(b.id), name = str(b.name); if (!id || !name) { issues.push("tool_use_missing_id_or_name"); continue; }
      const existing = calls.find((call) => call.id === id);
      if (existing) {
        addSource(existing.sources, "content");
        if (existing.name !== name || !sameJson(existing.arguments, b.input)) issues.push(`conflictingToolCall:${id}`);
        continue;
      }
      callIds.add(id); calls.push({ id, name, arguments: b.input, source: "content", sources: ["content"] });
    } else if (b.type === "tool_result") {
      const id = str(b.tool_use_id) ?? str(b.tool_call_id); if (!id) { issues.push("tool_result_missing_id"); continue; }
      const existing = results.find((result) => result.id === id);
      const execution = parseExecution(b, issues, bool(b.is_error), invalidExecutionIds, id);
      if (existing) {
        addSource(existing.sources, "content");
        existing.execution = mergeExecution(existing.execution, execution, issues, invalidExecutionIds, id);
        if (existing.content !== text(b.content) || existing.isError !== bool(b.is_error)) issues.push(`conflictingToolResult:${id}`);
        continue;
      }
      resultIds.add(id); results.push({ id, content: text(b.content), isError: bool(b.is_error), execution, source: "content", sources: ["content"] });
    }
  }
  if (Array.isArray(p?.tool_calls)) for (const raw of p!.tool_calls as unknown[]) {
    const c = obj(raw), f = obj(c?.function), id = str(c?.id), name = str(f?.name) ?? str(c?.name);
    if (!id || !name) { issues.push("tool_call_missing_id_or_name"); continue; }
    let args: unknown = f?.arguments ?? c?.arguments ?? c?.input;
    if (typeof args === "string") { try { args = JSON.parse(args); } catch { issues.push(`invalidArguments:${id}`); } }
    const existing = calls.find((call) => call.id === id);
    if (existing) {
      addSource(existing.sources, "tool_calls");
      if (existing.name !== name || !sameJson(existing.arguments, args)) issues.push(`conflictingToolCall:${id}`);
      continue;
    }
    callIds.add(id); calls.push({ id, name, arguments: args, source: "tool_calls", sources: ["tool_calls"] });
  }
  if (row.role === "tool") {
    const id = str(p?.tool_call_id) ?? str(p?.tool_use_id);
    const existing = id ? results.find((result) => result.id === id) : undefined;
    const execution = parseExecution(p ?? {}, issues, bool(p?.is_error), invalidExecutionIds, id ?? "");
    if (id && existing) {
      addSource(existing.sources, "message");
      existing.execution = mergeExecution(existing.execution, execution, issues, invalidExecutionIds, id);
      if (existing.content !== text(p?.content) || existing.isError !== bool(p?.is_error)) issues.push(`conflictingToolResult:${id}`);
    } else if (id) {
      resultIds.add(id); results.push({ id, content: text(p?.content), isError: bool(p?.is_error), execution, source: "message", sources: ["message"] });
    }
    else if (!id) issues.push("tool_message_missing_id");
  }
  for (const result of results) if (invalidExecutionIds.has(result.id)) result.execution = unknownExecution("typed");
  const payloadRole = str(p?.role);
  if (payloadRole && payloadRole !== row.role) issues.push("role_mismatch");
  return {
    messageId: row.message_id, threadId: row.thread_id, sequence: row.sequence, origin,
    role: row.role, text: text(p?.content), calls, results,
    excludedFromContext: row.excluded !== 0, truncated: row.truncated !== 0,
    isSummary: false, parseIssues: issues,
  };
}

export class DataLoader {
  private readonly db: Database;
  private statements = 0;
  readonly capabilities: SchemaCapabilities;
  constructor(readonly path: string = DEFAULT_DB_PATH) {
    this.db = new Database(path, { readonly: true });
    this.capabilities = this.inspectSchema();
    for (const [table, names] of Object.entries(REQUIRED)) {
      const missing = names.filter((n) => !this.capabilities.columns[table]?.includes(n));
      if (missing.length) { this.db.close(); throw new SchemaCompatibilityError(table, missing); }
    }
  }
  close(): void { this.db.close(); }
  /** All callback reads observe one deferred SQLite snapshot. */
  withSnapshot<T>(fn: (loader: this) => T): { value: T; statements: number } {
    const before = this.statements; this.db.exec("BEGIN");
    try { const value = fn(this); this.db.exec("COMMIT"); return { value, statements: this.statements - before }; }
    catch (e) { try { this.db.exec("ROLLBACK"); } catch { /* keep original error */ } throw e; }
  }
  private query<T>(sql: string, ...args: unknown[]): T[] { this.statements++; return this.db.query(sql).all(...args as any[]) as T[]; }
  private one<T>(sql: string, ...args: unknown[]): T | null { this.statements++; return (this.db.query(sql).get(...args as any[]) as T | null) ?? null; }
  private *iterateRows<T>(sql: string, ...args: unknown[]): Generator<T> {
    this.statements++;
    for (const row of this.db.query(sql).iterate(...args as any[])) yield row as T;
  }
  private inspectSchema(): SchemaCapabilities {
    const userVersion = this.one<{ user_version: number }>("PRAGMA user_version")?.user_version ?? 0;
    const tables = this.query<{ name: string }>("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").map((r) => r.name);
    const columns: Record<string, string[]> = {};
    for (const table of tables) columns[table] = this.query<{ name: string }>(`PRAGMA table_info("${table.replaceAll('"', '""')}")`).map((r) => r.name);
    const optionalMissing: Record<string, string[]> = {};
    for (const [table, names] of Object.entries(OPTIONAL)) optionalMissing[table] = names.filter((n) => !columns[table]?.includes(n));
    return { userVersion, tables, columns, optionalMissing };
  }
  private threadColumns(): string {
    const has = (n: string) => this.capabilities.columns.threads?.includes(n);
    const c = (n: string, fallback: string) => has(n) ? `t.${n}` : `${fallback} AS ${n}`;
    return ["t.id", "t.title", "t.cwd", "t.created_at", "t.updated_at", "t.message_count", c("parent_thread_id", "NULL"), c("snapshot_at_message_id", "NULL"), c("hidden", "0"), c("cancel_policy", "NULL"), c("config", "NULL"), c("cached_context", "NULL"), c("frozen_context", "NULL"), c("inherited_context", "NULL"), c("agent_status", "NULL"), c("context_cache_epoch", "NULL")].join(", ");
  }
  private threadSummaryColumns(): string {
    const has = (n: string) => this.capabilities.columns.threads?.includes(n);
    const c = (n: string, fallback: string) => has(n) ? `t.${n}` : `${fallback} AS ${n}`;
    return ["t.id", "t.title", "t.cwd", "t.created_at", "t.updated_at", "t.message_count", c("parent_thread_id", "NULL"), c("snapshot_at_message_id", "NULL"), c("hidden", "0"), c("cancel_policy", "NULL"), c("agent_status", "NULL"), c("context_cache_epoch", "NULL")].join(", ");
  }
  private messageColumns(): string {
    const has = (n: string) => this.capabilities.columns.messages?.includes(n);
    const c = (n: string, fallback: string) => has(n) ? `m.${n}` : `${fallback} AS ${n}`;
    return ["m.message_id", "m.thread_id", "m.role", "m.content", c("truncated", "0"), c("excluded", "0"), c("projection", "NULL"), "m.rowid AS sequence"].join(", ");
  }
  private threadPredicate(column: string, predicate: string, fallback: string): string { return this.capabilities.columns.threads?.includes(column) ? predicate : fallback; }
  private visiblePredicate(): string { return this.threadPredicate("hidden", "t.hidden=0", "1=1"); }
  private rootPredicate(): string { return this.threadPredicate("parent_thread_id", "t.parent_thread_id IS NULL", "1=1"); }
  loadVisibleThreads(): ThreadRow[] { return this.query<ThreadRow>(`SELECT ${this.threadColumns()} FROM threads t WHERE ${this.visiblePredicate()} ORDER BY t.created_at,t.id`); }
  loadThreadSummaries(): ThreadSummary[] { return this.query<ThreadSummary>(`SELECT ${this.threadSummaryColumns()} FROM threads t ORDER BY t.created_at,t.id`); }
  loadVisibleThreadSummaries(): ThreadSummary[] { return this.query<ThreadSummary>(`SELECT ${this.threadSummaryColumns()} FROM threads t WHERE ${this.visiblePredicate()} ORDER BY t.created_at,t.id`); }
  loadVisibleMainThreads(): ThreadRow[] { return this.query<ThreadRow>(`SELECT ${this.threadColumns()} FROM threads t WHERE ${this.visiblePredicate()} AND ${this.rootPredicate()} ORDER BY t.created_at,t.id`); }
  loadVisibleThreadsSince(hours: number): ThreadRow[] { return this.query<ThreadRow>(`SELECT ${this.threadColumns()} FROM threads t WHERE ${this.visiblePredicate()} AND t.updated_at>=? ORDER BY t.created_at,t.id`, new Date(Date.now() - hours * 3600000).toISOString()); }
  loadAllThreads(): ThreadRow[] { return this.query<ThreadRow>(`SELECT ${this.threadColumns()} FROM threads t ORDER BY t.created_at,t.id`); }
  loadAllMainThreads(): ThreadRow[] { return this.query<ThreadRow>(`SELECT ${this.threadColumns()} FROM threads t WHERE ${this.rootPredicate()} ORDER BY t.created_at,t.id`); }
  loadAllSubAgents(): ThreadRow[] { return this.capabilities.columns.threads?.includes("parent_thread_id") ? this.query<ThreadRow>(`SELECT ${this.threadColumns()} FROM threads t WHERE t.parent_thread_id IS NOT NULL ORDER BY t.created_at,t.id`) : []; }
  loadSubAgents(id: string): ThreadRow[] { return this.capabilities.columns.threads?.includes("parent_thread_id") ? this.query<ThreadRow>(`SELECT ${this.threadColumns()} FROM threads t WHERE t.parent_thread_id=? ORDER BY t.created_at,t.id`, id) : []; }
  loadThreadsByIds(ids: string[]): ThreadRow[] { const out: ThreadRow[] = []; for (let i=0;i<ids.length;i+=900) { const x=ids.slice(i,i+900); if (x.length) out.push(...this.query<ThreadRow>(`SELECT ${this.threadColumns()} FROM threads t WHERE t.id IN (${x.map(()=>"?").join(",")})`, ...x)); } return out; }
  hasThread(id: string): boolean { return this.one<{ present: number }>("SELECT 1 AS present FROM threads WHERE id=?", id) !== null; }
  loadMessages(threadId: string): MessageRow[] { return this.query<MessageRow>(`SELECT ${this.messageColumns()} FROM messages m WHERE m.thread_id=? ORDER BY m.rowid`, threadId); }
  loadNormalizedMessageWindow(threadId: string, messageId: string, radius: number): NormalizedMessage[] | null {
    const target = this.one<MessageRow>(`SELECT ${this.messageColumns()} FROM messages m WHERE m.thread_id=? AND m.message_id=?`, threadId, messageId);
    if (!target) return null;
    const before = this.query<MessageRow>(`SELECT ${this.messageColumns()} FROM messages m WHERE m.thread_id=? AND m.rowid<? ORDER BY m.rowid DESC LIMIT ?`, threadId, target.sequence, radius);
    const after = this.query<MessageRow>(`SELECT ${this.messageColumns()} FROM messages m WHERE m.thread_id=? AND m.rowid>? ORDER BY m.rowid ASC LIMIT ?`, threadId, target.sequence, radius);
    return [...before.reverse(), target, ...after].map((row) => normalizeMessage(row));
  }
  *iterateMessages(threadId?: string): Generator<MessageRow> { yield* (threadId === undefined ? this.iterateRows<MessageRow>(`SELECT ${this.messageColumns()} FROM messages m ORDER BY m.thread_id,m.rowid`) : this.iterateRows<MessageRow>(`SELECT ${this.messageColumns()} FROM messages m WHERE m.thread_id=? ORDER BY m.rowid`, threadId)); }
  *iterateNormalizedMessages(threadId?: string, origin: "own"|"inherited" = "own"): Generator<NormalizedMessage> { for (const row of this.iterateMessages(threadId)) yield normalizeMessage(row, origin); }
  loadNormalizedMessages(threadId: string): NormalizedMessage[] { return [...this.iterateNormalizedMessages(threadId)]; }
  loadInheritedMessages(threadId: string): NormalizedMessage[] {
    const t = this.loadThreadsByIds([threadId])[0];
    if (!t?.inherited_context) return [];
    let envelope: { version?: unknown; payloads?: unknown[]; flags?: unknown };
    try { envelope = JSON.parse(t.inherited_context); } catch { throw new InheritedContextError("invalidJson"); }
    if (envelope.version !== 1 || !Array.isArray(envelope.payloads) || !obj(envelope.flags)) throw new InheritedContextError("unsupportedEnvelope");
    const ids = new Set<string>();
    const messages: NormalizedMessage[] = [];
    for (let index = 0; index < envelope.payloads.length; index++) {
      const raw = envelope.payloads[index];
      if (typeof raw !== "string") throw new InheritedContextError(`payloadNotString:${index}`);
      let payload: Obj;
      try { payload = obj(JSON.parse(raw)) ?? {}; } catch { throw new InheritedContextError(`payloadInvalidJson:${index}`); }
      const versioned = "version" in payload || "type" in payload;
      if (versioned && payload.version !== 1) throw new InheritedContextError(`payloadUnsupportedVersion:${index}`);
      if (versioned && payload.type !== "message" && payload.type !== "system_reminder") throw new InheritedContextError(`payloadType:${index}`);
      const message = obj(payload.message);
      const id = payload.type === "system_reminder" ? str(payload.id) : versioned ? str(message?.id) : str(payload.id);
      if (!id || ids.has(id)) throw new InheritedContextError(`payloadId:${index}`);
      ids.add(id);
      const role = payload.type === "system_reminder" ? "system_reminder" : str(message?.role) ?? str(payload.role) ?? "unknown";
      const flag = obj((envelope.flags as Obj)[id]);
      const normalized = normalizeMessage({
        message_id: id, thread_id: threadId, role, content: raw,
        truncated: bool(flag?.truncated) === true ? 1 : 0,
        excluded: bool(flag?.excluded) === true ? 1 : 0,
        projection: flag?.projection === undefined ? null : JSON.stringify(flag.projection),
        sequence: index + 1,
      }, "inherited");
      messages.push(normalized);
    }
    for (const key of Object.keys(envelope.flags as object)) if (!ids.has(key)) throw new InheritedContextError(`flagReference:${key}`);
    return messages;
  }
  processMessages(id: string, fn: (row: MessageRow, index: number) => void): void { let i=0; for (const row of this.iterateMessages(id)) fn(row,i++); }
  private rowsWithToolErrors(rows: MessageRow[]): MessageRow[] {
    return rows.filter((row) => normalizeMessage(row).results.some((result) => result.isError === true));
  }
  loadToolErrors(): MessageRow[] {
    return this.rowsWithToolErrors(this.query<MessageRow>(`SELECT ${this.messageColumns()} FROM messages m ORDER BY m.rowid`));
  }
  loadToolErrorsForThread(id: string): MessageRow[] {
    return this.rowsWithToolErrors(this.query<MessageRow>(`SELECT ${this.messageColumns()} FROM messages m WHERE m.thread_id=? ORDER BY m.rowid`, id));
  }
  loadAssistantMessages(): MessageRow[] { return this.query<MessageRow>(`SELECT ${this.messageColumns()} FROM messages m WHERE m.role='assistant' ORDER BY m.rowid`); }
  getStats() {
    const visible = this.visiblePredicate().replaceAll("t.", "");
    const t = this.one<any>(`SELECT COUNT(*) total,SUM(${visible}) visible FROM threads`) ?? { total: 0, visible: 0 };
    const m = this.one<any>("SELECT COUNT(*) total FROM messages") ?? { total: 0 };
    const roles = this.query<any>("SELECT role,COUNT(*) count FROM messages GROUP BY role");
    const errors = this.loadToolErrors().length;
    return { totalThreads: t.total, visibleThreads: t.visible, totalMessages: m.total, roleDistribution: Object.fromEntries(roles.map((r) => [r.role, r.count])), totalToolErrors: errors };
  }
  getFilteredStats(hours: number) {
    const cutoff = new Date(Date.now() - hours * 3600000).toISOString();
    const visible = this.visiblePredicate();
    const t = this.one<any>(`SELECT COUNT(*) total,SUM(${visible}) visible FROM threads t WHERE t.updated_at>=?`, cutoff) ?? { total: 0, visible: 0 };
    const roles = this.query<any>(`SELECT m.role,COUNT(*) count FROM messages m JOIN threads t ON t.id=m.thread_id WHERE ${visible} AND t.updated_at>=? GROUP BY m.role`, cutoff);
    const n = this.one<any>(`SELECT COUNT(*) total FROM messages m JOIN threads t ON t.id=m.thread_id WHERE ${visible} AND t.updated_at>=?`, cutoff) ?? { total: 0 };
    const errorRows = this.query<MessageRow>(`SELECT ${this.messageColumns()} FROM messages m JOIN threads t ON t.id=m.thread_id WHERE ${visible} AND t.updated_at>=? ORDER BY m.rowid`, cutoff);
    return { totalThreads: t.total, visibleThreads: t.visible, totalMessages: n.total, roleDistribution: Object.fromEntries(roles.map((r) => [r.role, r.count])), totalToolErrors: this.rowsWithToolErrors(errorRows).length };
  }
  static parseContent(raw: string): unknown { try{return JSON.parse(raw);}catch{return null;} }
  static extractToolCalls(msg: unknown): ToolCall[] {
    const p = obj(msg); if (!p) return [];
    const message = p.type === "message" ? obj(p.message) : p;
    if (!message || (message.role !== "assistant" && p.type !== "message")) return [];
    return normalizeMessage({ message_id: "", thread_id: "", role: "assistant", content: JSON.stringify(p), truncated: 0, excluded: 0, projection: null, sequence: 0 }).calls;
  }
  static getToolUseBlocks(msg: unknown): unknown[] { const p=obj(msg); return Array.isArray(p?.content)?p.content.filter((b)=>obj(b)?.type==="tool_use"):[]; }
  static parseToolError(msg: unknown) { const p=obj(msg); if(!p||p.role!=="tool")return null; return {toolCallId:str(p.tool_call_id)??"",content:text(p.content),isError:bool(p.is_error)}; }
}
