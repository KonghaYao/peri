import { Database } from "bun:sqlite";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "bun:test";
import { parseCliArgs, run } from "./cli.js";

test("inspect CLI rejects missing and unknown arguments", () => {
  expect(() => parseCliArgs([])).toThrow("expected 'inspect'");
  expect(() => parseCliArgs(["inspect", "--db", "/tmp/x"])).toThrow("required");
  expect(() => parseCliArgs(["inspect", "--db", "/tmp/x", "--out", "/tmp/y", "--bad", "x"])).toThrow("unknown");
  expect(() => parseCliArgs(["inspect", "--db", "/tmp/no-such-peri-db", "--out", "/tmp/y"])).toThrow("does not exist");
  expect(() => parseCliArgs(["inspect", "--db", "/tmp/x", "--out", "/tmp/y", "--scope", "roots"])).toThrow("unknown");
});

test("task packet CLI exports sampled content, review report, and rejects invalid bounds", () => {
  const root = mkdtempSync(join(tmpdir(), "peri-task-cli-")); const dbPath = join(root, "threads.db"); const db = new Database(dbPath);
  db.exec(`CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER,parent_thread_id TEXT,hidden INTEGER DEFAULT 0); CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT,excluded INTEGER DEFAULT 0,truncated INTEGER DEFAULT 0,projection TEXT);`);
  db.query("INSERT INTO threads VALUES(?,?,?,?,?,?,?,?)").run("root", "root", "/tmp", "2026-08-15T00:00:00Z", "2026-08-15T00:00:01Z", 99, null, 0);
  db.query("INSERT INTO messages VALUES(?,?,?,?,?,?,?)").run("u1", "root", "user", JSON.stringify({ role: "user", content: "审计任务" }), 0, 0, null);
  db.query("INSERT INTO messages VALUES(?,?,?,?,?,?,?)").run("a1", "root", "assistant", JSON.stringify({ role: "assistant", content: "完成" }), 0, 0, null); db.close();
  const packetOut = join(root, "packets"); run(["task-sample", "--db", dbPath, "--out", packetOut, "--seed", "s", "--per-stratum", "1", "--include-content"]);
  const bundle = JSON.parse(readFileSync(join(packetOut, "task-packets.json"), "utf8")); expect(bundle.packets).toHaveLength(1); expect(bundle.packets[0].coverage.includeContent).toBe(true); expect(bundle.packets[0].messages[0].text).toBe("审计任务");
  const singleOut = join(root, "single"); run(["task-packet", "--db", dbPath, "--out", singleOut, "--thread", "root"]); const single = JSON.parse(readFileSync(join(singleOut, "task-packet.json"), "utf8")); expect(single.coverage.includeContent).toBe(false); expect(single.messages[0].text).toBeUndefined(); expect(single.thread.messageCount).toBe(99);
  const reviewInput = join(root, "reviews.json"); writeFileSync(reviewInput, JSON.stringify({ schemaVersion: 2, rubricVersion: "task-effectiveness-v1", reviews: [] })); const reviewOut = join(root, "review"); run(["task-review", "--packets", join(packetOut, "task-packets.json"), "--reviews", reviewInput, "--out", reviewOut]); expect(existsSync(join(reviewOut, "task-review.json"))).toBe(true);
  expect(() => parseCliArgs(["task-sample", "--db", dbPath, "--out", root, "--seed", "s", "--per-stratum", "0"])).toThrow("between 1 and 10"); expect(() => parseCliArgs(["task-sample", "--db", dbPath, "--out", root, "--seed", "s", "--bad", "x"])).toThrow("unknown");
});

test("report emits filtered metrics, rules, and bounded evidence", () => {
  const root = mkdtempSync(join(tmpdir(), "peri-report-"));
  const dbPath = join(root, "threads.db");
  const outDir = join(root, "out");
  const db = new Database(dbPath);
  db.exec(`
    CREATE TABLE threads (
      id TEXT PRIMARY KEY, title TEXT, cwd TEXT, created_at TEXT, updated_at TEXT,
      message_count INTEGER, parent_thread_id TEXT, hidden INTEGER DEFAULT 0
    );
    CREATE TABLE messages (
      message_id TEXT PRIMARY KEY, thread_id TEXT, role TEXT, content TEXT,
      excluded INTEGER DEFAULT 0, truncated INTEGER DEFAULT 0, projection TEXT
    );
  `);
  const addThread = db.query("INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?, ?, ?)");
  addThread.run("root", "root", "/private/project", "2026-01-01T00:00:00Z", "2026-01-01T00:00:01Z", 3, null, 0);
  addThread.run("child", "child", "/private/project", "2026-01-03T00:00:00Z", "2026-01-03T00:00:01Z", 2, "root", 1);
  const addMessage = db.query("INSERT INTO messages VALUES (?, ?, ?, ?, ?, ?, ?)");
  addMessage.run("m1", "root", "assistant", JSON.stringify({ role: "assistant", content: [{ type: "tool_use", id: "c1", name: "Read", input: { secret: "raw-argument" } }] }), 0, 0, null);
  addMessage.run("m2", "root", "tool", JSON.stringify({ role: "tool", tool_call_id: "c1", content: "é".repeat(10), is_error: true }), 0, 0, null);
  addMessage.run("m3", "root", "user", JSON.stringify({ role: "user", content: "continue" }), 0, 0, null);
  addMessage.run("m4", "child", "assistant", JSON.stringify({ role: "assistant", content: [{ type: "tool_use", id: "c2", name: "Hidden", input: {} }] }), 0, 0, null);
  addMessage.run("m5", "child", "tool", JSON.stringify({ role: "tool", tool_call_id: "c2", content: "hidden", is_error: false }), 0, 0, null);
  db.close();

  run(["report", "--db", dbPath, "--out", outDir, "--scope", "roots", "--since", "2026-01-01T00:00:00Z", "--until", "2026-01-02T00:00:00Z"]);
  const json = readFileSync(join(outDir, "report.json"), "utf8");
  const report = JSON.parse(json);
  expect(report.schema_version).toBe("report.v1");
  expect(report.filters).toEqual({ scope: "roots", includeHidden: false, since: "2026-01-01T00:00:00.000Z", until: "2026-01-02T00:00:00.000Z" });
  expect(report.totals.threads).toBe(1);
  expect(report.totals.explicitErrors).toBe(1);
  expect(report.top_tool_errors[0].label).toBe("Read");
  expect(report.top_tool_errors[0].errorAffectedThreads).toBe(1);
  expect(report.all_tools.map((tool: { label: string }) => tool.label)).toContain("Read");
  expect(report.rules.find((rule: { id: string }) => rule.id === "repeated-call").kind).toBe("candidate");
  expect(report.rules.find((rule: { id: string }) => rule.id === "explicit-failure").kind).toBe("candidate");
  expect(report.rules.find((rule: { id: string }) => rule.id === "large-tool-output").kind).toBe("observation");
  expect(report.rules.find((rule: { id: string }) => rule.id === "repeated-call").definition).toContain("longer run contributes one event");
  expect(report.result_bytes.p50).toBeGreaterThan(0);
  expect(report.capabilities.token_usage.status).toBe("unavailable");
  expect(json).not.toContain("raw-argument");
  expect(json).not.toContain("/private/project");

  const emptyOut = join(root, "empty");
  run(["report", "--db", dbPath, "--out", emptyOut, "--scope", "roots", "--since", "2027-01-01T00:00:00Z"]);
  const empty = JSON.parse(readFileSync(join(emptyOut, "report.json"), "utf8"));
  expect(empty.totals.threads).toBe(0);
  expect(empty.result_bytes.p50).toBeNull();
  expect(empty.pairing.pairing_rate).toBeNull();

  const sampleOut = join(root, "sample");
  run(["sample", "--db", dbPath, "--out", sampleOut, "--seed", "fixture", "--size", "1", "--scope", "roots"]);
  const sample = readFileSync(join(sampleOut, "sample.json"), "utf8");
  expect(JSON.parse(sample).samples).toHaveLength(1);
  expect(sample).not.toContain("raw-argument");

  const evidenceOut = join(root, "evidence");
  run(["evidence", "--db", dbPath, "--out", evidenceOut, "--thread", "root", "--message", "m1"]);
  const evidence = readFileSync(join(evidenceOut, "evidence.json"), "utf8");
  expect(JSON.parse(evidence).includeContent).toBe(false);
  expect(evidence).not.toContain("raw-argument");
  const contentOut = join(root, "content-evidence");
  run(["evidence", "--db", dbPath, "--out", contentOut, "--thread", "root", "--message", "m1", "--include-content"]);
  expect(readFileSync(join(contentOut, "evidence.json"), "utf8")).toContain("raw-argument");

  const compareOut = join(root, "compare");
  run(["compare", "--baseline", join(outDir, "report.json"), "--candidate", join(emptyOut, "report.json"), "--out", compareOut]);
  const comparison = JSON.parse(readFileSync(join(compareOut, "compare.json"), "utf8"));
  expect(comparison.filters.window_comparison).toBe("descriptive-different-creation-window");
  expect(readFileSync(join(compareOut, "compare.md"), "utf8")).toContain("Limitations");
  const malformed = join(root, "malformed.json"); const malformedOut = join(root, "malformed-compare");
  writeFileSync(malformed, "{ malformed");
  expect(() => run(["compare", "--baseline", malformed, "--candidate", join(outDir, "report.json"), "--out", malformedOut])).toThrow();
  expect(existsSync(join(malformedOut, "compare.json"))).toBe(false);
});

test("inspect reports an incompatible schema without reading raw data", () => {
  const root = mkdtempSync(join(tmpdir(), "peri-inspect-bad-schema-"));
  const dbPath = join(root, "threads.db");
  const outDir = join(root, "out");
  const db = new Database(dbPath);
  db.exec("CREATE TABLE threads(id TEXT); CREATE TABLE messages(message_id TEXT, thread_id TEXT, role TEXT, content TEXT);");
  db.close();
  run(["inspect", "--db", dbPath, "--out", outDir]);
  const dto = JSON.parse(readFileSync(join(outDir, "quality.json"), "utf8"));
  expect(dto.checks[0].status).toBe("fail");
  expect(dto.capabilities.normalized_messages.status).toBe("unavailable");
  expect(JSON.stringify(dto)).not.toContain(dbPath);
});

test("inspect emits quality facts without raw message, path, or args", () => {
  const root = mkdtempSync(join(tmpdir(), "peri-inspect-"));
  const dbPath = join(root, "threads.db");
  const outDir = join(root, "out");
  const db = new Database(dbPath);
  db.exec(`
    CREATE TABLE threads (
      id TEXT PRIMARY KEY, title TEXT, cwd TEXT, created_at TEXT, updated_at TEXT,
      message_count INTEGER, parent_thread_id TEXT, hidden INTEGER DEFAULT 0
    );
    CREATE TABLE messages (
      message_id TEXT PRIMARY KEY, thread_id TEXT, role TEXT, content TEXT, excluded INTEGER DEFAULT 0,
      truncated INTEGER DEFAULT 0, projection TEXT
    );
  `);
  const addThread = db.query("INSERT INTO threads VALUES (?, ?, ?, ?, ?, ?, ?, ?)");
  addThread.run("root", "root", "/private/secret/project", "2026-01-01T00:00:00Z", "2026-01-01T00:01:00Z", 4, null, 0);
  addThread.run("child", "child", "/private/secret/project", "2026-01-01T00:00:00Z", "2026-01-01T00:01:00Z", 4, "root", 1);
  const addMessage = db.query("INSERT INTO messages VALUES (?, ?, ?, ?, ?, ?, ?)");
  addMessage.run("m1", "root", "assistant", JSON.stringify({ role: "assistant", content: [{ type: "tool_use", id: "call-1", name: "Read", input: { path: "/private/raw/file", secret: "do-not-print" } }] }), 0, 0, null);
  addMessage.run("m2", "root", "tool", JSON.stringify({ role: "tool", tool_call_id: "call-1", content: "private result", is_error: true }), 0, 0, null);
  addMessage.run("m3", "root", "assistant", JSON.stringify({ version: 1, type: "message", message: { role: "assistant", content: [], tool_calls: [{ id: "call-2", name: "Grep", arguments: { pattern: "private-pattern" } }] } }), 0, 0, null);
  addMessage.run("m4", "root", "tool", JSON.stringify({ role: "tool", tool_call_id: "orphan", content: "orphan result", is_error: false }), 0, 0, null);
  addMessage.run("m5", "child", "assistant", JSON.stringify({ role: "assistant", content: [{ type: "tool_use", id: "call-3", name: "Bash", input: {} }], tool_calls: [{ id: "call-3", name: "Bash", arguments: {} }] }), 1, 1, "projection");
  addMessage.run("m6", "child", "user", JSON.stringify({ role: "user", content: "excluded user" }), 1, 0, null);
  addMessage.run("m7", "child", "system_reminder", JSON.stringify({ version: 1, type: "system_reminder", reminder: { body: "canonical reminder" } }), 0, 0, null);
  addMessage.run("m8", "child", "assistant", JSON.stringify({ version: 1, type: "future_message", message: { role: "assistant", content: "unknown envelope" } }), 0, 0, null);
  addMessage.run("m9", "child", "tool", JSON.stringify({ role: "tool", tool_call_id: "call-3", content: "unknown error state" }), 0, 0, null);
  addMessage.run("m10", "missing-thread", "user", JSON.stringify({ role: "user", content: "orphan row" }), 0, 0, null);
  db.close();

  run(["inspect", "--db", dbPath, "--out", outDir]);
  const json = readFileSync(join(outDir, "quality.json"), "utf8");
  const dto = JSON.parse(json);
  expect(dto.schema_version).toBe("inspect.v1");
  expect(dto.scope.threads).toEqual({ total: 2, roots: 1, children: 1, hidden: 1 });
  expect(dto.scope.messages.orphan_rows).toBe(1);
  expect(dto.capabilities.normalized_messages.status).toBe("warn");
  expect(dto.parsing.normalization_issues.unsupportedEnvelope).toBe(1);
  expect(dto.format.tool_result_unknown_error_count).toBe(1);
  expect(dto.canonical.excluded_messages).toBe(2);
  expect(dto.format.dual_write_messages).toBe(1);
  expect(dto.format.tool_use_count).toBe(3);
  expect(dto.format.duplicate_tool_use_candidates).toBe(1);
  expect(dto.format.tool_result_count).toBe(3);
  expect(dto.pairing.paired_results).toBe(2);
  expect(dto.pairing.orphan_results).toBe(1);
  expect(dto.capabilities.token_usage.status).toBe("unavailable");
  expect(json).not.toContain("/private/raw/file");
  expect(json).not.toContain("do-not-print");
  expect(json).not.toContain("private-pattern");
});
