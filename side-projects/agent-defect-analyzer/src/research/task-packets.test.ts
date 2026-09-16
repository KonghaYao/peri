import { Database } from "bun:sqlite";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "bun:test";
import { exportTaskPacket, sampleTaskPackets } from "./task-packets.js";
import { evidenceForMessage } from "./evidence.js";

function fixture(): string {
  const dir = mkdtempSync(join(tmpdir(), "peri-task-packets-")); const path = join(dir, "threads.db"); const db = new Database(path);
  db.exec(`CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER,parent_thread_id TEXT,hidden INTEGER DEFAULT 0,inherited_context TEXT); CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT,excluded INTEGER DEFAULT 0,truncated INTEGER DEFAULT 0,projection TEXT);`);
  const thread = db.query("INSERT INTO threads VALUES(?,?,?,?,?,?,?,?,?)");
  thread.run("short", "short", "/tmp", "2026-08-15T00:00:00Z", "2026-08-15T00:00:01Z", 2, null, 0, null);
  thread.run("empty", "empty", "/tmp", "2026-08-15T00:00:00Z", "2026-08-15T00:00:01Z", 0, null, 0, null);
  thread.run("child", "child", "/tmp", "2026-08-15T00:00:00Z", "2026-08-15T00:00:01Z", 1, "short", 0, null);
  const message = db.query("INSERT INTO messages VALUES(?,?,?,?,?,?,?)");
  message.run("u1", "short", "user", JSON.stringify({ role: "user", content: "你好🙂" }), 0, 0, null);
  message.run("a1", "short", "assistant", JSON.stringify({ role: "assistant", content: [{ type: "tool_use", id: "c1", name: "Echo", input: { value: "参数" } }] }), 0, 0, null);
  message.run("c1", "child", "user", JSON.stringify({ role: "user", content: "child" }), 0, 0, null);
  db.close(); return path;
}

test("task sampling is deterministic, stratified by actual own messages, and records empty candidates", () => {
  const path = fixture(); const first = sampleTaskPackets(path, { seed: "fixed", perStratum: 4, scope: "roots" }); const second = sampleTaskPackets(path, { seed: "fixed", perStratum: 4, scope: "roots" });
  expect(first.sampling.strata.short.selectedIds).toEqual(second.sampling.strata.short.selectedIds);
  expect(first.sampling.strata.short.candidateIds).toEqual(["short"]);
  expect(first.sampling.strata.medium.candidateCount).toBe(0); expect(first.sampling.noOwnMessages).toBe(1);
});

test("metadata packet excludes content while explicit content preserves Unicode and source flags", () => {
  const path = fixture(); const metadata = exportTaskPacket(path, "short"); const content = exportTaskPacket(path, "short", { includeContent: true });
  expect(metadata.coverage.includeContent).toBe(false); expect(metadata.messages[0].text).toBeUndefined(); expect(metadata.packetHash).not.toBe(content.packetHash);
  expect(content.messages[0].text).toBe("你好🙂"); expect(content.messages[1].calls[0].arguments).toEqual({ value: "参数" });
});

test("exports execution facts independently and protects output references by default", () => {
  const path = fixture(); const db = new Database(path);
  db.query("INSERT INTO messages VALUES(?,?,?,?,?,?,?)").run("r1", "short", "tool", JSON.stringify({ role: "tool", tool_call_id: "c1", content: "prefix", is_error: false, execution: { status: "completed", exit_code: 0, output_ref: "/tmp/private-output", output_truncated: true, task_id: "task-1" } }), 0, 0, null);
  db.close();
  const metadata = exportTaskPacket(path, "short");
  const metadataResult = metadata.messages.flatMap((message) => message.results)[0]!;
  expect(metadataResult.execution.status).toBe("completed");
  expect(metadataResult.execution.hasOutputRef).toBe(true);
  expect(metadataResult.execution.hasTaskId).toBe(true);
  expect(metadataResult.execution.outputRef).toBeUndefined();
  expect(metadataResult.execution.taskId).toBeUndefined();
  expect(JSON.stringify(metadata)).not.toContain("/tmp/private-output");
  const content = exportTaskPacket(path, "short", { includeContent: true });
  const contentResult = content.messages.flatMap((message) => message.results)[0]!;
  expect(contentResult.execution.outputRef).toBe("/tmp/private-output");
  expect(contentResult.execution.taskId).toBe("task-1");
});

test("execution facts survive transcript content truncation", () => {
  const path = fixture(); const db = new Database(path);
  db.query("INSERT INTO messages VALUES(?,?,?,?,?,?,?)").run("r1", "short", "tool", JSON.stringify({ role: "tool", tool_call_id: "c1", content: "x".repeat(20_000), is_error: false, execution: { status: "completed", exit_code: 0, output_ref: "/tmp/full", output_truncated: true } }), 0, 0, null);
  db.close();
  const packet = exportTaskPacket(path, "short", { includeContent: true, maxBytes: 1_500 });
  const result = packet.messages.flatMap((message) => message.results).find((candidate) => candidate.id === "c1");
  expect(result?.execution.status).toBe("completed");
  expect(result?.execution.outputTruncated).toBe(true);
  expect(result?.contentTruncated).toBe(true);
});

test("preserves the real Rust Bash failure capture through SQLite packet and evidence projections", () => {
  const capturePath = join(import.meta.dir, "./fixtures/rust-bash-failure.json");
  const capture = JSON.parse(readFileSync(capturePath, "utf8")) as unknown[];
  expect(capture).toHaveLength(1);
  const message = capture[0] as Record<string, unknown>;
  expect(message.version).toBeUndefined();
  expect((message.execution as Record<string, unknown>).status).toBe("failed");
  expect((message.execution as Record<string, unknown>).exit_code).toBe(7);
  const dir = mkdtempSync(join(tmpdir(), "peri-rust-capture-")); const path = join(dir, "threads.db"); const db = new Database(path);
  db.exec("CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER,parent_thread_id TEXT,hidden INTEGER DEFAULT 0); CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT,truncated INTEGER DEFAULT 0,excluded INTEGER DEFAULT 0,projection TEXT);");
  db.query("INSERT INTO threads VALUES(?,?,?,?,?,?,?,?)").run("capture", "capture", "/tmp", "2026-09-13T00:00:00Z", "2026-09-13T00:00:01Z", 1, null, 0);
  db.query("INSERT INTO messages VALUES(?,?,?,?,?,?,?)").run("capture-message", "capture", "tool", JSON.stringify(message), 0, 0, null); db.close();
  const packet = exportTaskPacket(path, "capture", { includeContent: true, maxBytes: 1_500 });
  const result = packet.messages.flatMap((item) => item.results)[0]!;
  expect(result.execution.status).toBe("failed"); expect(result.execution.exitCode).toBe(7); expect(result.execution.outputTruncated).toBe(true);
  expect(result.contentTruncated).toBe(true); expect(result.execution.outputRef).toBe("/redacted/peri-tool-output.txt");
  const metadata = exportTaskPacket(path, "capture"); const metadataJson = JSON.stringify(metadata);
  expect(metadataJson).not.toContain("/redacted/peri-tool-output.txt"); expect(metadata.messages[0]?.results[0]?.execution.hasOutputRef).toBe(true);
  const evidence = evidenceForMessage(path, { threadId: "capture", messageId: "capture-message", radius: 0, includeContent: true });
  const evidenceResult = evidence.records[0]?.content?.results[0]!;
  expect(evidenceResult.execution.status).toBe("failed"); expect(evidenceResult.execution.exitCode).toBe(7); expect(evidenceResult.execution.outputRef).toBe("/redacted/peri-tool-output.txt");
});

test("single packet reports omitted messages and remains within byte budget", () => {
  const path = fixture(); const packet = exportTaskPacket(path, "short", { includeContent: true, maxBytes: 700, maxMessages: 1 });
  expect(packet.coverage.exportTruncated).toBe(true); expect(packet.coverage.omittedMessages).toBeGreaterThan(0); expect(Buffer.byteLength(JSON.stringify(packet), "utf8")).toBeLessThanOrEqual(700);
});

test("inherited and own duplicate message ids are rejected", () => {
  const path = fixture(); const db = new Database(path); const inherited = JSON.stringify({ version: 1, payloads: [JSON.stringify({ version: 1, type: "message", message: { id: "u1", role: "user", content: "inherited" } })], flags: {} }); db.query("UPDATE threads SET inherited_context=? WHERE id=?").run(inherited, "short"); db.close();
  expect(() => exportTaskPacket(path, "short")).toThrow("duplicate message id");
});
