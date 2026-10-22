import { describe, expect, test } from "bun:test";
import type { SessionSummary } from "@peri-code/sdk/portable";
import { TursoChatSessionStore, emptyStoreTolerant } from "./store";

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

function session(overrides: Partial<SessionSummary> & Pick<SessionSummary, "id">): SessionSummary {
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

  test("有 schema 时原样返回", async () => {
    const storage = emptyStoreTolerant({
      getSessions: async () => [summary],
      getSession: async () => summary,
    });
    expect(await storage.getSessions("/workspace")).toEqual([summary]);
    expect(await storage.getSession(summary.id)).toEqual(summary);
  });
});

describe("TursoChatSessionStore", () => {
  test("空库首屏返回空列表", async () => {
    const store = new TursoChatSessionStore(emptyStoreTolerant({
      getSessions: async () => { throw missingSchema; },
      getSession: async () => { throw missingSchema; },
    }));

    expect(await store.list()).toEqual([]);
    expect(await store.get(summary.id)).toBeNull();
  });

  test("新旧 cwd 的会话都能列出并去重", async () => {
    const legacy = session({
      id: "01a0d141-99ba-76f0-8a94-aef2e08cdee1",
      cwd: "/workspace", updatedAt: "2026-10-20T12:00:00Z",
    });
    const current = session({
      id: "01a0d141-99ba-76f0-8a94-aef2e08cdee2",
      title: "当前会话", cwd: "/workspace/", updatedAt: "2026-10-20T13:00:00Z",
    });
    const duplicated = session({ ...legacy, title: "去重后的标题", cwd: "/workspace/" });
    const requested: string[] = [];
    const store = new TursoChatSessionStore({
      getSessions: async (cwd) => {
        requested.push(cwd);
        return cwd.endsWith("/") ? [current, duplicated] : [legacy];
      },
      getSession: async (id) => id === current.id ? current : legacy,
    });

    const chats = await store.list();
    expect(chats.map((chat) => chat.id)).toEqual([current.id, legacy.id]);
    expect(chats[1]?.title).toBe("去重后的标题");
    expect(requested.sort()).toEqual(["/workspace", "/workspace/"]);
    expect(await store.get(legacy.id)).not.toBeNull();
    expect(await store.get(current.id)).not.toBeNull();
  });

  test("按 updatedAt 倒序、同时间按 id 倒序", async () => {
    const older = session({
      id: "01a0d141-99ba-76f0-8a94-aef2e08cdee1", cwd: "/workspace",
      updatedAt: "2026-10-20T12:00:00Z",
    });
    const newer = session({
      id: "01a0d141-99ba-76f0-8a94-aef2e08cdee2", cwd: "/workspace/",
      updatedAt: "2026-10-20T13:00:00Z",
    });
    const tiedLow = session({
      id: "01a0d141-99ba-76f0-8a94-aef2e08cdee0", cwd: "/workspace",
      updatedAt: "2026-10-20T13:00:00Z",
    });
    const store = new TursoChatSessionStore({
      getSessions: async (cwd) => cwd.endsWith("/") ? [newer] : [older, tiedLow],
      getSession: async () => null,
    });

    expect((await store.list()).map((chat) => chat.id)).toEqual([newer.id, tiedLow.id, older.id]);
  });

  test("两次 cwd 查询串行执行，第一次完成前不发起第二次", async () => {
    let releaseLegacy!: (sessions: SessionSummary[]) => void;
    const pendingLegacy = new Promise<SessionSummary[]>((resolve) => { releaseLegacy = resolve; });
    const legacy = session({
      id: "01a0d141-99ba-76f0-8a94-aef2e08cdee1",
      cwd: "/workspace", updatedAt: "2026-10-20T12:00:00Z",
    });
    const current = session({
      id: "01a0d141-99ba-76f0-8a94-aef2e08cdee2",
      cwd: "/workspace/", updatedAt: "2026-10-20T13:00:00Z",
    });
    const requested: string[] = [];
    const store = new TursoChatSessionStore({
      getSessions: async (cwd) => {
        requested.push(cwd);
        return cwd.endsWith("/") ? [current] : pendingLegacy;
      },
      getSession: async () => null,
    });

    const listing = store.list();
    await Promise.resolve();
    expect(requested).toEqual(["/workspace"]);

    releaseLegacy([legacy]);
    expect((await listing).map((chat) => chat.id)).toEqual([current.id, legacy.id]);
    expect(requested).toEqual(["/workspace", "/workspace/"]);
  });

  test("其它 cwd 的会话不可读取", async () => {
    const store = new TursoChatSessionStore({
      getSessions: async () => [],
      getSession: async () => session({ id: summary.id, cwd: "/elsewhere" }),
    });

    expect(await store.get(summary.id)).toBeNull();
  });
});
