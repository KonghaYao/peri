import { describe, expect, test } from "bun:test";
import type { SessionSummary } from "@peri-code/sdk";
import { TursoChatDatabase, emptyStoreTolerant } from "./store";

const summary: SessionSummary = {
  id: "01a0d141-99ba-76f0-8a94-aef2e08cdedd",
  title: null,
  cwd: "/workspace",
  messageCount: 2,
  createdAt: "2026-10-20T12:00:00Z",
  updatedAt: "2026-10-20T12:00:00Z",
};

const missingSchema = new Error(
  "statement could not be prepared (SQL syntax, schema or statement shape)",
);

function session(
  overrides: Partial<SessionSummary> & Pick<SessionSummary, "id">,
): SessionSummary {
  return { ...summary, ...overrides };
}

describe("emptyStoreTolerant", () => {
  test("把缺表导致的 prepare 失败收敛为空库", async () => {
    const storage = emptyStoreTolerant({
      getSessions: async () => { throw missingSchema; },
      getSession: async () => { throw missingSchema; },
    });
    expect(await storage.getSessions("/workspace")).toEqual([]);
    expect(await storage.getSession(summary.id)).toBeNull();
  });

  test("其它存储错误照常抛出", async () => {
    const failure = new Error("store is unavailable: network error");
    const storage = emptyStoreTolerant({
      getSessions: async () => { throw failure; },
      getSession: async () => { throw failure; },
    });
    await expect(storage.getSessions("/workspace")).rejects.toThrow("store is unavailable");
    await expect(storage.getSession(summary.id)).rejects.toThrow("store is unavailable");
  });
});

describe("TursoChatDatabase", () => {
  test("空库首屏返回空目录", async () => {
    const database = new TursoChatDatabase(emptyStoreTolerant({
      getSessions: async () => { throw missingSchema; },
      getSession: async () => { throw missingSchema; },
    }));

    expect(await database.listSessions()).toEqual([]);
    expect(await database.getSession(summary.id)).toBeNull();
  });

  test("新旧 cwd 的目录合并去重并按时间倒序", async () => {
    const legacy = session({
      id: "01a0d141-99ba-76f0-8a94-aef2e08cdee1",
      cwd: "/workspace",
      updatedAt: "2026-10-20T12:00:00Z",
    });
    const current = session({
      id: "01a0d141-99ba-76f0-8a94-aef2e08cdee2",
      title: "当前会话",
      cwd: "/workspace/",
      updatedAt: "2026-10-20T13:00:00Z",
    });
    const duplicated = session({
      ...legacy,
      title: "当前 cwd 优先",
      cwd: "/workspace/",
    });
    const requested: string[] = [];
    const database = new TursoChatDatabase({
      getSessions: async (cwd) => {
        requested.push(cwd);
        return cwd.endsWith("/") ? [current, duplicated] : [legacy];
      },
      getSession: async (id) => id === current.id ? current : legacy,
    });

    const entries = await database.listSessions();
    expect(entries.map((entry) => entry.sessionId)).toEqual([current.id, legacy.id]);
    expect(entries.find((entry) => entry.sessionId === legacy.id)?.title).toBe("当前 cwd 优先");
    expect(requested).toEqual(["/workspace", "/workspace/"]);
    expect(entries[0]).not.toHaveProperty("running");
  });

  test("显式 path 只查询该目录并保留返回顺序", async () => {
    const older = session({
      id: "01a0d141-99ba-76f0-8a94-aef2e08cdee1",
      cwd: "/workspace",
      updatedAt: "2026-10-20T12:00:00Z",
    });
    const newer = session({
      id: "01a0d141-99ba-76f0-8a94-aef2e08cdee2",
      cwd: "/workspace/",
      updatedAt: "2026-10-20T13:00:00Z",
    });
    const requested: string[] = [];
    const database = new TursoChatDatabase({
      getSessions: async (cwd) => {
        requested.push(cwd);
        return cwd.endsWith("/") ? [newer] : [older];
      },
      getSession: async () => null,
    });

    const entries = await database.listSessions("/workspace/");
    expect(entries.map((entry) => entry.sessionId)).toEqual([newer.id]);
    expect(requested).toEqual(["/workspace/"]);
  });

  test("目录查询串行：第一次完成前不发起第二次", async () => {
    let releaseLegacy!: (sessions: SessionSummary[]) => void;
    const pendingLegacy = new Promise<SessionSummary[]>((resolve) => {
      releaseLegacy = resolve;
    });
    const requested: string[] = [];
    const database = new TursoChatDatabase({
      getSessions: async (cwd) => {
        requested.push(cwd);
        return cwd.endsWith("/") ? [] : pendingLegacy;
      },
      getSession: async () => null,
    });

    const listing = database.listSessions();
    await Promise.resolve();
    expect(requested).toEqual(["/workspace"]);
    releaseLegacy([]);
    expect(await listing).toEqual([]);
    expect(requested).toEqual(["/workspace", "/workspace/"]);
  });

  test("按 id 读取不追加 cwd 条件且保留可空标题", async () => {
    const database = new TursoChatDatabase({
      getSessions: async () => [],
      getSession: async () => session({ id: summary.id, cwd: "/elsewhere" }),
    });

    expect(await database.getSession(summary.id)).toEqual({
      sessionId: summary.id,
      title: null,
      cwd: "/elsewhere",
      messageCount: 2,
      createdAt: summary.createdAt,
      updatedAt: summary.updatedAt,
    });
  });

  test("存储不可达时目录查询向上抛错，不伪造空列表", async () => {
    const database = new TursoChatDatabase({
      getSessions: async () => { throw new Error("store is unavailable"); },
      getSession: async () => { throw new Error("store is unavailable"); },
    });
    await expect(database.listSessions()).rejects.toThrow("store is unavailable");
  });
});
