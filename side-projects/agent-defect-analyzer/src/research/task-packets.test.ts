import { Database } from "bun:sqlite";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "bun:test";
import { exportTaskPacket, sampleTaskPackets } from "./task-packets.js";

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

test("single packet reports omitted messages and remains within byte budget", () => {
  const path = fixture(); const packet = exportTaskPacket(path, "short", { includeContent: true, maxBytes: 700, maxMessages: 1 });
  expect(packet.coverage.exportTruncated).toBe(true); expect(packet.coverage.omittedMessages).toBeGreaterThan(0); expect(Buffer.byteLength(JSON.stringify(packet), "utf8")).toBeLessThanOrEqual(700);
});

test("inherited and own duplicate message ids are rejected", () => {
  const path = fixture(); const db = new Database(path); const inherited = JSON.stringify({ version: 1, payloads: [JSON.stringify({ version: 1, type: "message", message: { id: "u1", role: "user", content: "inherited" } })], flags: {} }); db.query("UPDATE threads SET inherited_context=? WHERE id=?").run(inherited, "short"); db.close();
  expect(() => exportTaskPacket(path, "short")).toThrow("duplicate message id");
});
