import { afterEach, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { analyzeShellWaiting } from "./waiting.js";
import { parseCliArgs, run } from "../cli.js";

const dirs: string[] = [];
afterEach(() => { for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true }); });

function makeDb(): string {
  const dir = mkdtempSync(join(tmpdir(), "peri-waiting-"));
  dirs.push(dir);
  const path = join(dir, "db.sqlite");
  const db = new Database(path);
  db.exec(`CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER,parent_thread_id TEXT,hidden INTEGER DEFAULT 0);
    CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT,truncated INTEGER DEFAULT 0,excluded INTEGER DEFAULT 0,projection TEXT);`);
  const thread = db.query("INSERT INTO threads VALUES (?,?,?,?,?,?,?,?)");
  thread.run("root", "root", "/tmp", "2026-01-01T00:00:00.000Z", "2026-01-01T00:10:00.000Z", 12, null, 0);
  thread.run("child", "child", "/tmp", "2026-01-15T00:00:00.000Z", "2026-01-15T00:10:00.000Z", 2, "root", 1);
  thread.run("hidden", "hidden", "/tmp", "2026-02-01T00:00:00.000Z", "2026-02-01T00:10:00.000Z", 1, null, 1);
  const message = db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)");
  const add = (id: string, threadId: string, role: string, value: unknown) => message.run(id, threadId, role, JSON.stringify(value), 0, 0, null);
  const call = (id: string, threadId: string, messageId: string, tool: string, input: unknown, extra: Record<string, unknown> = {}) =>
    add(messageId, threadId, "assistant", { role: "assistant", content: [{ type: "tool_use", id, name: tool, input }], ...extra });
  const result = (id: string, threadId: string, callId: string, status: string, isError: boolean | null) =>
    add(id, threadId, "tool", { role: "tool", tool_call_id: callId, content: "ok", is_error: isError, execution: { status } });

  call("c1", "root", "a1", "Bash", { command: "sleep 30; echo waited" }, {
    tool_calls: [{ id: "c1", name: "Bash", arguments: { command: "sleep 30; echo waited" } }],
  });
  result("r1", "root", "c1", "completed", false);
  call("c2", "root", "a2", "Bash", { command: "sleep 300", timeout: 120000, run_in_background: true });
  result("r2", "root", "c2", "timed_out", true);
  call("c3", "root", "a3", "Bash", { command: "sleep 1; tail -3 build.log" });
  result("r3", "root", "c3", "failed", true);
  call("c4", "root", "a4", "Bash", { command: "sleep 5" });
  call("c5", "root", "a5", "Read", { path: "/tmp/x" });
  call("c6", "root", "a6", "Bash", { command: 'echo "sleep 999"' });
  call("c7", "root", "a7", "Bash", { command: "bash -c 'sleep 5'" });
  call("c8", "root", "a8", "Bash", { command: "sleep $DELAY; true" });
  result("r8", "root", "c8", "completed", false);
  call("c9", "child", "ac", "Bash", { command: "sleep 2; grep -q ok log" });
  result("rc", "child", "c9", "completed", false);
  call("c10", "hidden", "hc", "Bash", { command: "sleep 3" });
  result("rh", "hidden", "c10", "unknown", null);
  db.close();
  return path;
}

test("counts, classifies, and pairs sleep calls in the default root scope", () => {
  const report = analyzeShellWaiting(makeDb());
  expect(report.totals.threads).toBe(1);
  expect(report.totals.commandCalls).toBe(7);
  expect(report.sleep.calls).toBe(5);
  expect(report.sleep.shareOfCommandCalls).toBe(0.714286);
  expect(report.sleep.threadsWithSleep).toBe(1);
  expect(report.sleep.threadShare).toBe(1);
  expect(report.sleep.byShape).toEqual({ standalone: 2, compound: 2, loop: 0, variable: 1 });
  expect(report.sleep.byWaitKind).toEqual({ pure: 2, noop_only: 2, other_command: 1 });
  expect(report.sleep.byExecutionStatus).toMatchObject({ completed: 2, failed: 1, timed_out: 1, missing: 1 });
  expect(report.sleep.byIsError).toEqual({ true: 2, false: 2, unknown: 1 });
  expect(report.totals.missingSleepResults).toBe(1);
  expect(report.sleep.backgroundSleepCalls).toBe(1);
  expect(report.sleep.sleepExceedingTimeoutCalls).toBe(1);
  expect(report.sleep.totalSleepSeconds).toBe(336);
  expect(report.sleep.callSleepSeconds).toEqual({ p50: 5, p90: 30, max: 300, knownSamples: 4 });
  expect(report.sensitivity.broadTextMatchCalls).toBe(5);
  expect(report.sensitivity.wrapperEmbeddedSleepCalls).toBe(1);
  expect(report.sleep.examples).toHaveLength(1);
});

test("applies scope, hidden, and date filters", () => {
  const path = makeDb();
  const all = analyzeShellWaiting(path, { scope: "all", includeHidden: true });
  expect(all.totals.threads).toBe(3);
  expect(all.totals.commandCalls).toBe(9);
  expect(all.sleep.calls).toBe(7);
  expect(all.sleep.byThreadType.visible_root).toMatchObject({ commandCalls: 7, sleepCalls: 5, threadsWithSleep: 1 });
  expect(all.sleep.byThreadType.child).toMatchObject({ commandCalls: 1, sleepCalls: 1 });
  expect(all.sleep.byThreadType.hidden_root).toMatchObject({ commandCalls: 1, sleepCalls: 1 });
  expect(all.sleep.byMonth["2026-01"]).toEqual({ commandCalls: 8, sleepCalls: 6, sleepShare: 0.75 });
  expect(all.sleep.byMonth["2026-02"]).toEqual({ commandCalls: 1, sleepCalls: 1, sleepShare: 1 });
  expect(all.sleep.byWeek["2025-12-29"]).toEqual({ commandCalls: 7, sleepCalls: 5, sleepShare: 0.714286 });
  expect(all.sleep.byWeek["2026-01-12"]).toEqual({ commandCalls: 1, sleepCalls: 1, sleepShare: 1 });
  expect(all.sleep.byWeek["2026-01-26"]).toEqual({ commandCalls: 1, sleepCalls: 1, sleepShare: 1 });

  const february = analyzeShellWaiting(path, { scope: "all", includeHidden: true, since: "2026-02-01T00:00:00.000Z" });
  expect(february.totals.threads).toBe(1);
  expect(february.totals.commandCalls).toBe(1);
  expect(february.sleep.calls).toBe(1);
});

test("produces a deterministic fingerprint for identical input", () => {
  const path = makeDb();
  expect(analyzeShellWaiting(path).source.fingerprint).toBe(analyzeShellWaiting(path).source.fingerprint);
});

test("waiting CLI writes JSON and Markdown reports", () => {
  const path = makeDb();
  const out = join(mkdtempSync(join(tmpdir(), "peri-waiting-out-")), "report");
  const args = parseCliArgs(["waiting", "--db", path, "--out", out, "--scope", "all", "--include-hidden"]);
  expect(args).toMatchObject({ command: "waiting", scope: "all", includeHidden: true });
  run(["waiting", "--db", path, "--out", out, "--scope", "all", "--include-hidden"]);
  expect(existsSync(join(out, "waiting.json"))).toBe(true);
  expect(existsSync(join(out, "waiting.md"))).toBe(true);
  expect(existsSync(join(out, "waiting.html"))).toBe(true);
  const dto = JSON.parse(readFileSync(join(out, "waiting.json"), "utf8"));
  expect(dto.schema_version).toBe("waiting.v1");
  expect(dto.sleep.calls).toBe(7);
  const markdown = readFileSync(join(out, "waiting.md"), "utf8");
  expect(markdown).toContain("# Shell sleep waiting report");
  expect(markdown).toContain("sleep_share");
  const html = readFileSync(join(out, "waiting.html"), "utf8");
  expect(html).toContain("<title>睡眠 shell 等待分析</title>");
  expect(html).toContain("按时间讨论");
  expect(html).toContain("版本归因限制");
});
