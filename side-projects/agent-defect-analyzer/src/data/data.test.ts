import { describe, expect, test, afterEach } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { DataLoader, SchemaCompatibilityError, normalizeMessage } from "./loader.js";
import type { MessageRow } from "./types.js";

const dirs: string[] = [];
afterEach(() => { for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true }); });
function dbWith(schema: string): string {
  const dir = mkdtempSync(join(tmpdir(), "peri-analyzer-")); dirs.push(dir);
  const path = join(dir, "threads.db"); const db = new Database(path); db.exec(schema); db.close(); return path;
}
const row = (content: unknown, role = "assistant"): MessageRow => ({ message_id: "m1", thread_id: "t1", role, content: JSON.stringify(content), truncated: 0, excluded: 0, projection: null, sequence: 1 });

test("normalizes Anthropic and OpenAI dual writes once, retaining unknown error", () => {
  const message = normalizeMessage(row({ role: "assistant", content: [{ type: "tool_use", id: "c1", name: "Read", input: ["a"] }], tool_calls: [{ id: "c1", function: { name: "Read", arguments: "[\"a\"]" }, arguments: { duplicate: true } }] }));
  expect(message.calls).toHaveLength(1);
  expect(message.calls[0]?.arguments).toEqual(["a"]);
  const result = normalizeMessage(row({ role: "tool", tool_call_id: "c1", content: "ok" }, "tool"));
  expect(result.results[0]?.isError).toBeNull();
});

test("retains typed execution evidence and never upgrades a string-shaped header", () => {
  const typed = normalizeMessage(row({ role: "tool", tool_call_id: "c1", content: "exit 0", is_error: false, execution: {
    status: "completed", exit_code: 0, output_ref: "/tmp/full-output", output_truncated: true, task_id: "task-1",
  } }, "tool"));
  expect(typed.results[0]?.execution).toEqual({ status: "completed", exitCode: 0, outputRef: "/tmp/full-output", outputTruncated: true, taskId: "task-1", source: "typed" });
  const legacy = normalizeMessage(row({ role: "tool", tool_call_id: "c2", content: "[execution] status=completed exit_code=0", is_error: false }, "tool"));
  expect(legacy.results[0]?.execution.status).toBe("unknown");
  expect(legacy.results[0]?.execution.source).toBe("legacy");
});

test("keeps illegal and contradictory execution metadata observable", () => {
  const message = normalizeMessage(row({ role: "tool", tool_call_id: "c1", content: "failed", is_error: false, execution: {
    status: "failed", exit_code: "0", output_truncated: "yes", task_id: 4, extra: true,
  } }, "tool"));
  expect(message.results[0]?.execution.status).toBe("unknown");
  expect(message.parseIssues).toEqual(expect.arrayContaining([
    "invalidExecutionExitCode", "invalidExecutionOutputTruncated", "invalidExecutionTaskId", "unknownExecutionField:extra", "conflictingExecutionErrorFlag",
  ]));
});

test("normalizes bash terminal lifecycle statuses without inferring from exit text", () => {
  const cases = [
    ["completed", 0, false], ["failed", 2, true], ["running", null, false], ["cancelled", null, true],
  ] as const;
  for (const [status, exitCode, isError] of cases) {
    const message = normalizeMessage(row({ role: "tool", tool_call_id: status, content: `exit_code=${exitCode ?? "?"}`, is_error: isError, execution: { status, exit_code: exitCode } }, "tool"));
    expect(message.results[0]?.execution.status).toBe(status);
    expect(message.parseIssues).toEqual([]);
  }
  const legacy = normalizeMessage(row({ role: "tool", tool_call_id: "legacy", content: "process exited with code 0" }, "tool"));
  expect(legacy.results[0]?.execution.status).toBe("unknown");
});

test("rejects contradictory execution facts from dual persisted representations", () => {
  const message = normalizeMessage(row({ role: "tool", tool_call_id: "c1", content: [
    { type: "tool_result", tool_use_id: "c1", content: "ok", is_error: false, execution: { status: "completed", exit_code: 0 } },
  ], execution: { status: "failed", exit_code: 3 }, is_error: true }, "tool"));
  expect(message.results[0]?.execution.status).toBe("unknown");
  expect(message.parseIssues).toContain("conflictingExecutionMetadata:c1");
});

test("treats reordered JSON object keys as the same dual write", () => {
  const message = normalizeMessage(row({
    role: "assistant",
    content: [{ type: "tool_use", id: "c1", name: "Read", input: { path: "a", line: 1 } }],
    tool_calls: [{ id: "c1", function: { name: "Read", arguments: { line: 1, path: "a" } } }],
  }));
  expect(message.calls).toHaveLength(1);
  expect(message.parseIssues).not.toContain("conflictingToolCall:c1");
  expect(message.calls[0]?.sources).toEqual(["content", "tool_calls"]);
});

test("unwraps persisted V1 message envelopes and keeps reminders out of summaries", () => {
  const message = normalizeMessage(row({ version: 1, type: "message", message: { id: "m1", role: "user", content: "hello" } }, "user"));
  expect(message.text).toBe("hello");
  const reminder = normalizeMessage(row({ version: 1, type: "system_reminder", id: "r1", reminder: {} }, "system_reminder"));
  expect(reminder.isSummary).toBe(false);
  expect(reminder.parseIssues).toHaveLength(0);
});

test("keeps malformed JSON and role mismatch observable", () => {
  const bad = normalizeMessage({ ...row("not-json"), content: "not-json" });
  expect(bad.parseIssues).toContain("invalidJson");
  const mismatch = normalizeMessage(row({ role: "user", content: "hello" }));
  expect(mismatch.parseIssues).toContain("role_mismatch");
  expect(normalizeMessage(row({})).parseIssues).toContain("payload_missing_role_or_content");
});

test("supports old required-only schema with nullable optional projections", () => {
  const path = dbWith(`CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER);
    CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT);
    INSERT INTO threads VALUES ('t1','title','/tmp','2026-01-01','2026-01-01',1);
    INSERT INTO messages VALUES ('m1','t1','user','{"role":"user","content":"hi"}');`);
  const loader = new DataLoader(path);
  const thread = loader.loadAllThreads()[0];
  expect(thread?.parent_thread_id).toBeNull(); expect(thread?.hidden).toBe(0);
  expect(loader.loadMessages("t1")[0]?.excluded).toBe(0);
  loader.close();
});

test("read-only loader and snapshot transaction", () => {
  const path = dbWith(`CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER, parent_thread_id TEXT, hidden INTEGER);
    CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT,excluded INTEGER);
    INSERT INTO threads VALUES ('t1','title','/tmp','2026-01-01','2026-01-01',1,NULL,0);
    INSERT INTO messages VALUES ('m1','t1','user','{"role":"user","content":"hi"}',0);`);
  const loader = new DataLoader(path);
  const snap = loader.withSnapshot((db) => [db.loadAllThreads().length, [...db.iterateMessages()].length]);
  expect(snap.value).toEqual([1, 1]); expect(snap.statements).toBeGreaterThan(0);
  expect(() => (loader as any).db.exec("UPDATE threads SET title='x'")).toThrow();
  loader.close();
});

test("missing required columns fails explicitly", () => {
  const path = dbWith("CREATE TABLE threads(id TEXT); CREATE TABLE messages(message_id TEXT,thread_id TEXT,role TEXT,content TEXT);");
  expect(() => new DataLoader(path)).toThrow(SchemaCompatibilityError);
});

test("inherited payloads remain separately scoped", () => {
  const path = dbWith(`CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER,parent_thread_id TEXT,hidden INTEGER,inherited_context TEXT);
    CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT);
    INSERT INTO threads VALUES ('t1','title','/tmp','2026-01-01','2026-01-01',0,NULL,0,'{"version":1,"payloads":["{\\"version\\":1,\\"type\\":\\"message\\",\\"message\\":{\\"id\\":\\"old-1\\",\\"role\\":\\"user\\",\\"content\\":\\"old\\"}}"],"flags":{}}');`);
  const loader = new DataLoader(path);
  expect(loader.loadMessages("t1")).toHaveLength(0);
  expect(loader.loadInheritedMessages("t1")[0]?.origin).toBe("inherited");
  loader.close();
});

test("stats and errors inspect V1 content blocks, including unknown errors", () => {
  const path = dbWith(`CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER,hidden INTEGER);
    CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT);
    INSERT INTO threads VALUES ('t1','title','/tmp','2026-01-01','2026-01-01',2,0);
    INSERT INTO messages VALUES ('m1','t1','assistant','{"version":1,"type":"message","message":{"id":"m1","role":"assistant","content":[{"type":"tool_result","tool_use_id":"c1","content":"failed","is_error":true}]}}');
    INSERT INTO messages VALUES ('m2','t1','tool','{"version":1,"type":"message","message":{"id":"m2","role":"tool","tool_call_id":"c2","content":"unknown"}}');`);
  const loader = new DataLoader(path);
  expect(loader.loadToolErrors()).toHaveLength(1);
  expect(loader.getStats().totalToolErrors).toBe(1);
  loader.close();
});
