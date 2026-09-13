import { describe, expect, test, afterEach } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { createApp } from "./app.js";
import { loadConfig } from "./server.js";
import { ViewerDataAdapter } from "./data_adapter.js";
import type { NormalizedViewerMessage, ViewerDataSource, ViewerThread } from "./routes/api.js";

const thread: ViewerThread = {
  id: "thread-1",
  title: "fixture",
  cwd: "/tmp/project",
  created_at: "2026-01-01T00:00:00.000Z",
  updated_at: "2026-01-01T00:00:01.000Z",
  message_count: 3,
  parent_thread_id: null,
  hidden: 0,
  agent_status: "done",
};

const messages: NormalizedViewerMessage[] = [
  {
    messageId: "m1",
    threadId: thread.id,
    sequence: 1,
    origin: "own",
    role: "assistant",
    text: "",
    calls: [
      { id: "call-1", name: "Read", arguments: { path: "a.rs" }, source: "tool_calls" },
      { id: "call-2", name: "Grep", arguments: { pattern: "needle" }, source: "content" },
    ],
    results: [],
    excludedFromContext: false,
    truncated: false,
    isSummary: false,
    parseIssues: [],
  },
  {
    messageId: "m2",
    threadId: thread.id,
    sequence: 2,
    origin: "own",
    role: "tool",
    text: "",
    calls: [],
    results: [
      { id: "call-1", content: "missing file", isError: true, source: "message" },
    ],
    excludedFromContext: false,
    truncated: false,
    isSummary: false,
    parseIssues: [],
  },
];

const tempDirs: string[] = [];
afterEach(() => { for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true }); });

function fixtureSource(): ViewerDataSource {
  return {
    close() {},
    getStats: () => ({ totalThreads: 1, visibleThreads: 1, totalMessages: 3, roleDistribution: { assistant: 1, tool: 1 }, totalToolErrors: 1 }),
    getAgentStatusDist: () => [{ agent_status: "done", count: 1 }],
    loadAllSubAgents: () => [],
    getTimeline: () => [{ date: "2026-01-01", count: 1 }],
    getDistinctCwds: () => [thread.cwd],
    loadThreadsPaginated: () => [thread],
    getThreadCount: () => 1,
    loadSubAgents: () => [],
    getThreadById: (id) => id === thread.id ? thread : null,
    loadMessages: (id) => id === thread.id ? messages : [],
    loadMessagesPage: (id, offset, limit) => id === thread.id ? messages.slice(offset, offset + limit) : [],
    getMessageCount: (id) => id === thread.id ? messages.length : 0,
    getToolStats: () => [{ name: "Read", count: 1, resultCount: 1, knownResultCount: 1, unknownResultCount: 0, errorCount: 1, errorRate: 100 }],
    getRecentToolErrors: () => [{ msg_rowid: 2, thread_id: thread.id, content: "missing file", role: "tool", thread_title: thread.title }],
    searchMessages: () => ({ rows: [{ thread_id: thread.id, role: "assistant", content: "needle", thread_title: thread.title }], total: 1 }),
  };
}

async function json(response: Response): Promise<any> {
  return response.json();
}

describe("viewer API contract", () => {
  test("fails clearly for a missing or incompatible database", async () => {
    await expect(ViewerDataAdapter.open("/tmp/peri-viewer-does-not-exist.db")).rejects.toThrow();
    const dir = mkdtempSync(join(tmpdir(), "peri-viewer-"));
    tempDirs.push(dir);
    const path = join(dir, "bad.db");
    const db = new Database(path);
    db.exec("CREATE TABLE threads(id TEXT); CREATE TABLE messages(message_id TEXT,thread_id TEXT,role TEXT,content TEXT);");
    db.close();
    await expect(ViewerDataAdapter.open(path)).rejects.toThrow(/incompatible session schema/);
  });

  test("uses shared normalization for top-level calls, dual writes and error pairing", async () => {
    const dir = mkdtempSync(join(tmpdir(), "peri-viewer-"));
    tempDirs.push(dir);
    const path = join(dir, "fixture.db");
    const db = new Database(path);
    db.exec(`
      CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER,parent_thread_id TEXT,hidden INTEGER,agent_status TEXT);
      CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT,truncated INTEGER,excluded INTEGER,projection TEXT);
      INSERT INTO threads VALUES ('t1','fixture','/tmp','2026-01-01','2026-01-01',2,NULL,0,'done');
      INSERT INTO messages VALUES ('m1','t1','assistant','{"role":"assistant","content":[{"type":"tool_use","id":"c1","name":"Read","input":{"path":"a"}}],"tool_calls":[{"id":"c1","function":{"name":"Read","arguments":"{\\"path\\":\\"a\\"}"}}]}',0,0,NULL);
      INSERT INTO messages VALUES ('m2','t1','tool','{"role":"tool","tool_call_id":"c1","content":"missing","is_error":true}',0,0,NULL);
    `);
    db.close();
    const source = await ViewerDataAdapter.open(path);
    try {
      const normalized = source.loadMessages("t1");
      expect(normalized[0]?.calls).toHaveLength(1);
      expect(normalized[1]?.results[0]).toMatchObject({ id: "c1", isError: true });
      expect(source.getToolStats()).toEqual([{ name: "Read", count: 1, resultCount: 1, knownResultCount: 1, unknownResultCount: 0, errorCount: 1, errorRate: 100 }]);
      expect(source.getRecentToolErrors(1)[0]).toMatchObject({ msg_rowid: 2, thread_id: "t1", content: "missing" });
    } finally {
      source.close();
    }
  });

  test("loads a local-only configuration with an explicit database path", () => {
    expect(loadConfig({ PERI_DB_VIEWER_PORT: "9000", PERI_DB_VIEWER_HOST: "0.0.0.0", PERI_DB_PATH: "/tmp/fixture.db" })).toEqual({
      host: "127.0.0.1",
      port: 9000,
      dbPath: "/tmp/fixture.db",
    });
    expect(() => loadConfig({ PERI_DB_VIEWER_PORT: "0" })).toThrow();
  });

  test("serves dashboard, timeline, cwds, threads and tool stats", async () => {
    const app = createApp(fixtureSource());
    const chart = await app.request("/assets/echarts.min.js");
    expect(chart.status).toBe(200);
    expect(chart.headers.get("content-type")).toContain("application/javascript");
    expect((await chart.text()).length).toBeGreaterThan(100_000);
    expect((await json(await app.request("/api/stats"))).totalThreads).toBe(1);
    expect((await json(await app.request("/api/timeline?days=7")))[0].date).toBe("2026-01-01");
    expect((await json(await app.request("/api/cwds")))[0]).toBe(thread.cwd);
    expect((await json(await app.request("/api/threads?perPage=1"))).rows[0].subagent_count).toBe(0);
    expect((await json(await app.request("/api/tools/stats"))).errorRate[0].errorCount).toBe(1);
  });

  test("returns details, normalized messages and search results", async () => {
    const app = createApp(fixtureSource());
    const detail = await json(await app.request(`/api/threads/${thread.id}`));
    expect(detail.thread.id).toBe(thread.id);
    const detailMessages = await json(await app.request(`/api/threads/${thread.id}/messages`));
    expect(detailMessages.messages[0].calls.map((call: any) => call.id)).toEqual(["call-1", "call-2"]);
    expect(detailMessages.messages[1].results[0]).toMatchObject({ id: "call-1", isError: true });
    expect(detailMessages).toMatchObject({ total: 2, offset: 0, perPage: 100, hasMore: false });
    const firstMessagePage = await json(await app.request(`/api/threads/${thread.id}/messages?page=1&perPage=1`));
    const secondMessagePage = await json(await app.request(`/api/threads/${thread.id}/messages?page=2&perPage=1`));
    expect(firstMessagePage).toMatchObject({ total: 2, offset: 0, perPage: 1, hasMore: true });
    expect(secondMessagePage).toMatchObject({ total: 2, offset: 1, perPage: 1, hasMore: false });
    const search = await json(await app.request("/api/search?q=needle&perPage=1"));
    expect(search.total).toBe(1);
  });

  test("rejects missing threads and invalid bounded pagination", async () => {
    const app = createApp(fixtureSource());
    expect((await app.request("/api/threads/missing/messages")).status).toBe(404);
    expect((await app.request("/api/threads?page=0")).status).toBe(400);
    expect((await app.request("/api/threads?perPage=101")).status).toBe(400);
    expect((await app.request("/api/search?q=x&page=nope")).status).toBe(400);
    expect((await app.request("/api/timeline?days=0")).status).toBe(400);
    expect((await app.request("/api/threads/thread-1/messages?perPage=501")).status).toBe(400);
  });

test("rejects missing search query", async () => {
  const app = createApp(fixtureSource());
  expect((await app.request("/api/search")).status).toBe(400);
});

test("does not count inherited parent errors as child errors", async () => {
  const dir = mkdtempSync(join(tmpdir(), "peri-viewer-inherited-"));
  tempDirs.push(dir);
  const path = join(dir, "fixture.db");
  const db = new Database(path);
  const inherited = JSON.stringify({
    version: 1,
    payloads: [JSON.stringify({ version: 1, type: "message", message: { id: "parent-error", role: "tool", tool_call_id: "parent-call", content: "failed", is_error: true } })],
    flags: {},
  });
  db.exec(`
    CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER,parent_thread_id TEXT,hidden INTEGER,agent_status TEXT,inherited_context TEXT);
    CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT,truncated INTEGER,excluded INTEGER,projection TEXT);
  `);
  db.query("INSERT INTO threads VALUES (?,?,?,?,?,?,?,?,?,?)").run("parent", "parent", "/tmp", "2026-01-01", "2026-01-01", 1, null, 0, "done", null);
  db.query("INSERT INTO threads VALUES (?,?,?,?,?,?,?,?,?,?)").run("child", "child", "/tmp", "2026-01-02", "2026-01-02", 1, "parent", 0, "done", inherited);
  db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)").run("parent-row", "parent", "tool", JSON.stringify({ role: "tool", tool_call_id: "parent-call", content: "failed", is_error: true }), 0, 0, null);
  db.query("INSERT INTO messages VALUES (?,?,?,?,?,?,?)").run("child-row", "child", "assistant", JSON.stringify({ role: "assistant", content: "own" }), 0, 0, null);
  db.close();

  const source = await ViewerDataAdapter.open(path);
  try {
    expect(source.getStats().totalToolErrors).toBe(1);
    const errors = source.getRecentToolErrors(10);
    expect(errors).toHaveLength(1);
    expect(errors[0]).toMatchObject({ thread_id: "parent", msg_rowid: 1 });
    expect(errors[0].msg_rowid).toBeGreaterThan(0);
  } finally { source.close(); }
});

test("keeps unknown and missing result calls out of the known error denominator", async () => {
  const dir = mkdtempSync(join(tmpdir(), "peri-viewer-unknown-"));
  tempDirs.push(dir);
  const path = join(dir, "fixture.db");
  const db = new Database(path);
  db.exec(`
    CREATE TABLE threads(id TEXT PRIMARY KEY,title TEXT,cwd TEXT,created_at TEXT,updated_at TEXT,message_count INTEGER);
    CREATE TABLE messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,content TEXT);
    INSERT INTO threads VALUES ('t1','fixture','/tmp','2026-01-01','2026-01-01',3);
  `);
  const add = db.query("INSERT INTO messages VALUES (?,?,?,?)");
  add.run("call-row", "t1", "assistant", JSON.stringify({ role: "assistant", content: [{ type: "tool_use", id: "unknown-call", name: "UnknownTool", input: {} }, { type: "tool_use", id: "missing-call", name: "MissingTool", input: {} }] }));
  add.run("unknown-result", "t1", "tool", JSON.stringify({ role: "tool", tool_call_id: "unknown-call", content: "undetermined" }));
  db.close();

  const source = await ViewerDataAdapter.open(path);
  try {
    const stats = Object.fromEntries(source.getToolStats().map((row) => [row.name, row]));
    expect(stats.UnknownTool).toMatchObject({ count: 1, resultCount: 1, knownResultCount: 0, unknownResultCount: 1, errorRate: null });
    expect(stats.MissingTool).toMatchObject({ count: 1, resultCount: 0, knownResultCount: 0, unknownResultCount: 0, errorRate: null });
  } finally { source.close(); }
});
});
