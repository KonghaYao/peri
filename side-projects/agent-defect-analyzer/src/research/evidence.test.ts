import { afterEach, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { evidenceForMessage, EvidenceInputError, EvidenceNotFoundError, sampleThreads } from "./evidence.js";

const dirs: string[] = [];
afterEach(() => { for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true }); });
function fixture(): string {
  const dir = mkdtempSync(join(tmpdir(), "peri-evidence-")); dirs.push(dir);
  const path = join(dir, "db.sqlite"); const db = new Database(path);
  db.exec(`CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER,parent_thread_id TEXT,hidden INTEGER DEFAULT 0);
    CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT,truncated INTEGER DEFAULT 0,excluded INTEGER DEFAULT 0,projection TEXT);`);
  const t = db.query("INSERT INTO threads VALUES (?,?,?,?,?,?,?,?)");
  t.run("root-a", "", "/private", "2026-01-01T00:00:00.000Z", "2026-01-01T00:01:00.000Z", 3, null, 0);
  t.run("root-b", "", "/private", "2026-01-02T00:00:00.000Z", "2026-01-02T00:01:00.000Z", 1, null, 0);
  t.run("child", "", "/private", "2026-01-03T00:00:00.000Z", "2026-01-03T00:01:00.000Z", 1, "root-a", 0);
  t.run("hidden", "", "/private", "2026-01-04T00:00:00.000Z", "2026-01-04T00:01:00.000Z", 1, null, 1);
  const m = db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)");
  m.run("u", "root-a", "user", JSON.stringify({ role: "user", content: "private text" }), 0, 0, null);
  m.run("a", "root-a", "assistant", JSON.stringify({ role: "assistant", content: [{ type: "tool_use", id: "call-a", name: "Read", input: { path: "x".repeat(100000) } }] }), 0, 0, null);
  m.run("r", "root-a", "tool", JSON.stringify({ role: "tool", tool_call_id: "call-a", content: "x".repeat(100000), is_error: false, execution: { status: "completed", exit_code: 0, output_ref: "/tmp/private-output", output_truncated: true, task_id: "task-1" } }), 0, 0, null);
  db.close(); return path;
}

test("seeded sampling is deterministic and samples the eligible population", () => {
  const path = fixture();
  const first = sampleThreads(path, { seed: "same", size: 2, scope: "roots" });
  const again = sampleThreads(path, { seed: "same", size: 2, scope: "roots" });
  const other = sampleThreads(path, { seed: "different", size: 2, scope: "roots" });
  expect(first).toEqual(again);
  expect(first.eligibleCount).toBe(2);
  expect(first.samples.every((sample) => sample.threadId.startsWith("root-"))).toBe(true);
  expect(other.samples.map((sample) => sample.threadId)).not.toEqual(first.samples.map((sample) => sample.threadId));
  expect(JSON.stringify(first)).not.toContain("private");
});

test("sampling honors children, hidden, and half-open creation filters", () => {
  const path = fixture();
  expect(sampleThreads(path, { seed: 1, size: 1, scope: "children" }).samples[0]?.threadId).toBe("child");
  expect(sampleThreads(path, { seed: 1, size: 3, scope: "all", includeHidden: true, since: "2026-01-02T00:00:00Z", until: "2026-01-04T00:00:00Z" }).eligibleCount).toBe(2);
});

test("message evidence defaults to metadata and never crosses the thread", () => {
  const path = fixture();
  const result = evidenceForMessage(path, { threadId: "root-a", messageId: "a", radius: 1 });
  expect(result.records.map((record) => record.messageId)).toEqual(["u", "a", "r"]);
  expect(result.records.every((record) => record.content === undefined)).toBe(true);
  const resultFacts = result.records.find((record) => record.messageId === "r")?.results[0]?.execution;
  expect(resultFacts?.hasOutputRef).toBe(true);
  expect(resultFacts?.outputRef).toBeUndefined();
  expect(resultFacts?.taskId).toBeUndefined();
  expect(JSON.stringify(result)).not.toContain("secret");
  expect(() => evidenceForMessage(path, { threadId: "child", messageId: "a" })).toThrow(EvidenceNotFoundError);
});

test("content evidence is bounded and reports truncation", () => {
  const result = evidenceForMessage(fixture(), { threadId: "root-a", messageId: "r", radius: 0, includeContent: true });
  expect(Buffer.byteLength(JSON.stringify(result, null, 2), "utf8")).toBeLessThanOrEqual(64 * 1024);
  expect(result.truncated).toBe(true);
  expect(result.records[0]?.content).toBeDefined();
  expect(result.records[0]?.content?.results[0]?.contentTruncated).toBe(true);
  expect(result.records[0]?.content?.results[0]?.execution.outputRef).toBe("/tmp/private-output");

  const call = evidenceForMessage(fixture(), { threadId: "root-a", messageId: "a", radius: 0, includeContent: true });
  expect(call.records[0]?.content?.calls[0]?.argumentsTruncated).toBe(true);
  expect(Buffer.byteLength(call.records[0]?.content?.calls[0]?.arguments ?? "", "utf8")).toBeLessThanOrEqual(16 * 1024);
});

test("bounds dense call metadata while retaining the target identity", () => {
  const path = fixture();
  const db = new Database(path);
  const blocks = Array.from({ length: 2000 }, (_, index) => ({ type: "tool_use", id: `call-${index}`, name: "Read", input: { index } }));
  db.query("UPDATE messages SET content=? WHERE message_id='a'").run(JSON.stringify({ role: "assistant", content: blocks }));
  db.close();
  const result = evidenceForMessage(path, { threadId: "root-a", messageId: "a", radius: 0, includeContent: true });
  expect(result.targetMessageId).toBe("a");
  expect(result.records[0]?.messageId).toBe("a");
  expect(result.omissions.calls).toBeGreaterThan(0);
  expect(result.truncated).toBe(true);
  expect(Buffer.byteLength(JSON.stringify(result, null, 2), "utf8")).toBeLessThanOrEqual(64 * 1024);
});

test("invalid ranges and missing identifiers fail explicitly", () => {
  const path = fixture();
  expect(() => sampleThreads(path, { seed: 1, size: 0 })).toThrow(EvidenceInputError);
  expect(() => sampleThreads(path, { seed: 1, size: 1, since: "2026-01-03", until: "2026-01-02" })).toThrow(EvidenceInputError);
  expect(() => sampleThreads(path, { seed: 1, size: 1, since: "2026-02-31T00:00:00Z" })).toThrow(EvidenceInputError);
  expect(() => sampleThreads(path, { seed: 1, size: 1, since: "2026-01-01" })).toThrow(EvidenceInputError);
  expect(() => sampleThreads(path, { seed: 1, size: 1, scope: "invalid" as never })).toThrow(EvidenceInputError);
  const db = new Database(path);
  db.query("UPDATE threads SET created_at='not-a-date' WHERE id='root-a'").run();
  db.close();
  expect(() => sampleThreads(path, { seed: 1, size: 1 })).toThrow("invalid created_at");
  expect(() => evidenceForMessage(path, { threadId: "missing", messageId: "a" })).toThrow(EvidenceNotFoundError);
  expect(() => evidenceForMessage(path, { threadId: "root-a", messageId: "missing" })).toThrow(EvidenceNotFoundError);
});
