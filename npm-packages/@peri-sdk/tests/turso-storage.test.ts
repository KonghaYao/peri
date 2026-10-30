import { describe, expect, test } from "bun:test";
import type { Connection } from "@tursodatabase/serverless";
import type { ChatSessionDirectoryEntry } from "../src/contracts/chat";
import { TursoStorage, mergeSessionDirectories } from "../src/storage/turso-storage";

function entry(
  sessionId: string,
  cwd: string,
  updatedAt: string,
  title = sessionId,
): ChatSessionDirectoryEntry {
  return {
    sessionId,
    title,
    cwd,
    messageCount: 0,
    createdAt: updatedAt,
    updatedAt,
  };
}

describe("TursoStorage storageConfig", () => {
  test("returns plain JSON with only connection keys", () => {
    const storage = new TursoStorage({
      url: "http://127.0.0.1:8081",
      authToken: "secret",
      legacyCwd: "/workspace",
      currentCwd: "/workspace/",
    });

    const config = storage.storageConfig;

    expect(Object.getPrototypeOf(config)).toBe(Object.prototype);
    expect(Object.keys(config).sort()).toEqual(["authToken", "url"]);
    expect(config).toEqual({ url: "http://127.0.0.1:8081", authToken: "secret" });
    expect(config).not.toHaveProperty("legacyCwd");
    expect(config).not.toHaveProperty("currentCwd");
    expect(JSON.parse(JSON.stringify(config))).toEqual(config);
    expect(storage.storageConfig).not.toBe(config);
    expect(storage.storageConfig).toEqual(config);
  });

  test("omits authToken when undefined", () => {
    const config = new TursoStorage({ url: "http://127.0.0.1:8081" }).storageConfig;

    expect(Object.keys(config)).toEqual(["url"]);
    expect(config).toEqual({ url: "http://127.0.0.1:8081" });
    expect(JSON.stringify(config)).toBe('{"url":"http://127.0.0.1:8081"}');
  });
});

describe("TursoStorage session directory", () => {
  test("merges legacy and current cwd; current wins on duplicate id", () => {
    const legacy = [
      entry("a", "/workspace", "2026-01-01T00:00:00Z", "legacy"),
      entry("b", "/workspace", "2026-01-03T00:00:00Z"),
    ];
    const current = [
      entry("a", "/workspace/", "2026-01-02T00:00:00Z", "current"),
      entry("c", "/workspace/", "2026-01-04T00:00:00Z"),
    ];

    expect(mergeSessionDirectories(legacy, current)).toEqual([
      entry("c", "/workspace/", "2026-01-04T00:00:00Z"),
      entry("b", "/workspace", "2026-01-03T00:00:00Z"),
      entry("a", "/workspace/", "2026-01-02T00:00:00Z", "current"),
    ]);
  });

  test("orders equal timestamps by sessionId desc", () => {
    const legacy = [
      entry("a", "/workspace", "2026-01-01T00:00:00Z"),
      entry("b", "/workspace", "2026-01-01T00:00:00Z"),
    ];

    expect(
      mergeSessionDirectories(legacy, []).map((item) => item.sessionId),
    ).toEqual(["b", "a"]);
  });
});

type SessionRow = {
  id: string;
  title: string | null;
  cwd: string;
  message_count: number;
  created_at: string;
  updated_at: string;
};

function sessionRow(id: string, cwd = "/workspace"): SessionRow {
  return {
    id,
    title: id,
    cwd,
    message_count: 0,
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
  };
}

/** 假连接：记录建连/关闭次数与同时在途的查询数。 */
function connectionStub(rows: SessionRow[]) {
  const opened: string[] = [];
  let closes = 0;
  let active = 0;
  let maxActive = 0;
  return {
    opened,
    closes: () => closes,
    maxActive: () => maxActive,
    connector: (config: { url: string; authToken?: string }): Connection => {
      opened.push(config.url);
      return {
        all: async () => {
          active += 1;
          maxActive = Math.max(maxActive, active);
          await new Promise((resolve) => setTimeout(resolve, 1));
          active -= 1;
          return rows;
        },
        close: async () => {
          closes += 1;
        },
      } as unknown as Connection;
    },
  };
}

describe("TursoStorage connection lifecycle", () => {
  test("reuses one connection across queries and closes it once", async () => {
    const stub = connectionStub([sessionRow("a")]);
    const storage = new TursoStorage(
      { url: "http://127.0.0.1:8081" },
      stub.connector,
    );

    expect(await storage.getSession("a")).toMatchObject({ sessionId: "a" });
    await storage.listSessions("/workspace");
    await storage.listSessions("/workspace/");
    expect(stub.opened).toEqual(["http://127.0.0.1:8081"]);

    await storage.close();
    await storage.close();
    expect(stub.closes()).toBe(1);
  });

  test("serializes concurrent queries on the single stream", async () => {
    const stub = connectionStub([sessionRow("a")]);
    const storage = new TursoStorage(
      { url: "http://127.0.0.1:8081" },
      stub.connector,
    );

    await Promise.all([
      storage.getSession("a"),
      storage.getSession("b"),
      storage.getSession("c"),
    ]);
    expect(stub.maxActive()).toBe(1);
  });

  test("close waits for the in-flight query and rejects later queries", async () => {
    const stub = connectionStub([sessionRow("a")]);
    const storage = new TursoStorage(
      { url: "http://127.0.0.1:8081" },
      stub.connector,
    );

    const pending = storage.getSession("a");
    const closing = storage.close();
    await expect(pending).resolves.toMatchObject({ sessionId: "a" });
    await closing;
    expect(stub.closes()).toBe(1);
    await expect(storage.getSession("a")).rejects.toThrow(
      "TursoStorage is closed",
    );
  });
});
