import { afterEach, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { analyzeDatabase } from "./metrics.js";

const dirs: string[] = [];
afterEach(() => { for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true }); });
function makeDb(): string {
  const dir = mkdtempSync(join(tmpdir(), "peri-metrics-")); dirs.push(dir);
  const path = join(dir, "db.sqlite");
  const db = new Database(path);
  db.exec(`CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER,parent_thread_id TEXT,hidden INTEGER DEFAULT 0);
    CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT,truncated INTEGER DEFAULT 0,excluded INTEGER DEFAULT 0,projection TEXT);`);
  const thread = db.query("INSERT INTO threads VALUES (?,?,?,?,?,?,?,?)");
  thread.run("root", "root", "/tmp", "2026-01-01T00:00:00.000Z", "2026-01-01T00:10:00.000Z", 9, null, 0);
  thread.run("child", "child", "/tmp", "2026-01-02T00:00:00.000Z", "2026-01-02T00:10:00.000Z", 1, "root", 0);
  thread.run("hidden", "hidden", "/tmp", "2026-01-03T00:00:00.000Z", "2026-01-03T00:10:00.000Z", 1, null, 1);
  const message = db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)");
  const add = (id: string, threadId: string, role: string, value: unknown) => message.run(id, threadId, role, JSON.stringify(value), 0, 0, null);
  add("u1", "root", "user", { role: "user", content: "go" });
  for (const id of ["c1", "c2", "c3"]) add(`a${id}`, "root", "assistant", { role: "assistant", content: [{ type: "tool_use", id, name: "Read", input: { path: "same" } }] });
  add("a4", "root", "assistant", { role: "assistant", content: [{ type: "tool_use", id: "x1", name: "ExecuteExtraTool", input: { tool_name: "CronRegister", params: {} } }] });
  add("r1", "root", "tool", { role: "tool", tool_call_id: "c1", content: "failed", is_error: true });
  add("r2", "root", "tool", { role: "tool", tool_call_id: "c2", content: "failed", is_error: true });
  add("r3", "root", "tool", { role: "tool", tool_call_id: "c3", content: "ok", is_error: false });
  add("r4", "root", "tool", { role: "tool", tool_call_id: "unknown", content: "orphan", is_error: true });
  add("rx", "root", "tool", { role: "tool", tool_call_id: "x1", content: "?" });
  add("ac", "child", "assistant", { role: "assistant", content: [{ type: "tool_use", id: "child-call", name: "Read", input: {} }] });
  add("hc", "hidden", "assistant", { role: "assistant", content: [{ type: "tool_use", id: "hidden-call", name: "Read", input: {} }] });
  db.query("UPDATE messages SET content=? WHERE message_id='r3'").run(JSON.stringify({ role: "tool", tool_call_id: "c3", content: "x".repeat(100000), is_error: false }));
  db.close();
  return path;
}

function appendLongRuns(path: string): void {
  const db = new Database(path);
  const message = db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)");
  const add = (id: string, value: { role: string; [key: string]: unknown }) => message.run(id, "root", value.role, JSON.stringify(value), 0, 0, null);
  for (let index = 1; index <= 4; index++) {
    add(`long-call-${index}`, { role: "assistant", content: [{ type: "tool_use", id: `long-${index}`, name: "Read", input: { path: "long" } }] });
    add(`long-result-${index}`, { role: "tool", tool_call_id: `long-${index}`, content: "ok", is_error: false });
  }
  for (let index = 1; index <= 3; index++) {
    add(`fail-call-${index}`, { role: "assistant", content: [{ type: "tool_use", id: `fail-${index}`, name: "FailTool", input: { path: `failure-${index}` } }] });
    add(`fail-result-${index}`, { role: "tool", tool_call_id: `fail-${index}`, content: "failed", is_error: true });
  }
  db.close();
}

test("root analysis pairs per thread, separates wrappers, and bounds candidates", () => {
  const report = analyzeDatabase(makeDb());
  expect(report.filters.scope).toBe("roots");
  expect(report.totals.threads).toBe(1);
  expect(report.totals.pairedResults).toBe(4);
  expect(report.totals.pairedKnownResults).toBe(3);
  expect(report.totals.orphanResults).toBe(1);
  expect(report.totals.unknownErrorResults).toBe(1);
  expect(report.tools.Read.errorRate).toBeCloseTo(2 / 3);
  expect(report.tools["ExecuteExtraTool→CronRegister"]?.outerName).toBe("ExecuteExtraTool");
  expect(report.resultBytes.giantCount).toBe(1);
  expect(report.resultBytes.p50).not.toBeNull();
  const repeated = report.candidates.find((candidate) => candidate.ruleId === "repeated-call");
  expect(repeated?.counts).toBe(1);
  expect(repeated?.denominator).toBe(4);
  expect(repeated?.evidence.entries).toHaveLength(1);
});

test("counts execution coverage and statuses separately from isError error rates", () => {
  const path = makeDb(); const db = new Database(path);
  db.query("UPDATE messages SET content=? WHERE message_id='r1'").run(JSON.stringify({ role: "tool", tool_call_id: "c1", content: "failed", is_error: true, execution: { status: "failed", exit_code: 9, output_truncated: false } }));
  db.close();
  const report = analyzeDatabase(path);
  expect(report.totals.execution?.resultCount).toBe(5);
  expect(report.totals.execution?.typedCount).toBe(1);
  expect(report.totals.execution?.knownCount).toBe(1);
  expect(report.totals.execution?.coverage).toBeCloseTo(1 / 5);
  expect(report.totals.execution?.statusCounts.failed).toBe(1);
  // Existing tool error rate remains based on explicit is_error and known results.
  expect(report.tools.Read.errorRate).toBeCloseTo(2 / 3);
});

test("malformed and double-written execution metadata stays unknown in metric status counts", () => {
  const path = makeDb(); const db = new Database(path);
  db.query("UPDATE messages SET content=? WHERE message_id='r1'").run(JSON.stringify({ role: "tool", tool_call_id: "c1", content: "failed", is_error: false, execution: { status: "completed", exit_code: 7 } }));
  db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)").run("conflict-result", "root", "tool", JSON.stringify({ role: "tool", tool_call_id: "c2", content: [{ type: "tool_result", tool_use_id: "c2", content: "ok", execution: { status: "completed", exit_code: 0 } }], execution: { status: "failed", exit_code: 2 } }), 0, 0, null);
  db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)").run("conflict-call", "root", "assistant", JSON.stringify({ role: "assistant", content: [{ type: "tool_use", id: "c2", name: "Conflict", input: {} }] }), 0, 0, null);
  db.close();
  const report = analyzeDatabase(path);
  expect(report.parseIssues.conflictingExecutionExitCode).toBe(1);
  expect(report.parseIssues.conflictingExecutionMetadata).toBe(1);
  expect(report.totals.execution?.knownCount).toBe(0);
  expect(report.totals.execution?.statusCounts.unknown).toBe(6);
  expect(report.totals.execution?.statusCounts.completed).toBe(0);
});

test("long consecutive runs contribute one threshold event", () => {
  const path = makeDb();
  const before = analyzeDatabase(path);
  appendLongRuns(path);
  const after = analyzeDatabase(path);
  const count = (report: ReturnType<typeof analyzeDatabase>, ruleId: string): number => report.candidates.find((candidate) => candidate.ruleId === ruleId)?.counts ?? 0;
  expect(count(after, "repeated-call") - count(before, "repeated-call")).toBe(1);
  expect(count(after, "explicit-failure") - count(before, "explicit-failure")).toBe(1);
});

test("scope, hidden filter, and half-open creation window are explicit", () => {
  const path = makeDb();
  expect(analyzeDatabase(path, { scope: "children" }).totals.threads).toBe(1);
  expect(analyzeDatabase(path, { scope: "all", includeHidden: true }).totals.threads).toBe(3);
  expect(analyzeDatabase(path, { scope: "all", since: "2026-01-02T00:00:00.000Z", until: "2026-01-03T00:00:00.000Z" }).totals.threads).toBe(1);
});

test("analysis windows require ISO timestamps with an explicit timezone", () => {
  const path = makeDb();
  expect(() => analyzeDatabase(path, { since: "2026-01-01" })).toThrow("timezone");
  expect(() => analyzeDatabase(path, { until: "2026-01-02T00:00:00" })).toThrow("timezone");
  expect(() => analyzeDatabase(path, { since: "2026-01-02T00:00:00+08:00" })).not.toThrow();
});

test("same id repeated after settlement is counted separately without creating missing result", () => {
  const path = makeDb();
  const db = new Database(path);
  db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)").run("dup", "root", "assistant", JSON.stringify({ role: "assistant", content: [{ type: "tool_use", id: "c1", name: "Read", input: { path: "same" } }] }), 0, 0, null);
  db.close();
  const report = analyzeDatabase(path);
  expect(report.totals.duplicateCallIds).toBe(1);
  expect(report.totals.missingResults).toBe(0);
});

test("empty result sample has null quantiles", () => {
  const dir = mkdtempSync(join(tmpdir(), "peri-empty-")); dirs.push(dir);
  const path = join(dir, "empty.db"); const db = new Database(path);
  db.exec("CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER); CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT);");
  db.query("INSERT INTO threads VALUES (?,?,?,?,?,?)").run("t", "t", "/tmp", "2026-01-01", "2026-01-01", 0); db.close();
  const report = analyzeDatabase(path);
  expect(report.resultBytes.p50).toBeNull(); expect(report.resultBytes.p95).toBeNull();
});

test("pairs calls and results within a thread, even when IDs are reused", () => {
  const path = makeDb();
  const db = new Database(path);
  db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)").run("child-call", "child", "assistant", JSON.stringify({ role: "assistant", content: [{ type: "tool_use", id: "c1", name: "Read", input: {} }] }), 0, 0, null);
  db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)").run("child-result", "child", "tool", JSON.stringify({ role: "tool", tool_call_id: "c1", content: "child", is_error: false }), 0, 0, null);
  db.close();
  const report = analyzeDatabase(path, { scope: "all" });
  expect(report.totals.missingResults).toBe(1);
  expect(report.totals.orphanResults).toBe(1);
});

test("does not treat conflicting normalized records as valid evidence", () => {
  const path = makeDb();
  const db = new Database(path);
  const payload = { role: "assistant", content: [
    { type: "tool_use", id: "conflict", name: "Read", input: { path: "a" } },
    { type: "tool_use", id: "conflict", name: "Read", input: { path: "b" } },
  ] };
  db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)").run("conflict-message", "root", "assistant", JSON.stringify(payload), 0, 0, null);
  db.close();
  const report = analyzeDatabase(path);
  expect(report.parseIssues.conflictingToolCall).toBe(1);
  expect(report.tools.Read.calls).toBe(4);
  expect(report.totals.missingResults).toBe(0);
});

test("rejects an invalid thread creation date instead of silently dropping it", () => {
  const path = makeDb();
  const db = new Database(path);
  db.query("UPDATE threads SET created_at='not-a-date' WHERE id='root'").run();
  db.close();
  expect(() => analyzeDatabase(path)).toThrow("invalid created_at");
});
