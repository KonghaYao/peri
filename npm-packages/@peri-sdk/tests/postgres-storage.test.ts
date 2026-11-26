import { describe, expect, test } from "bun:test";
import type { DirectoryConnection as Connection } from "../src/storage/postgres-storage";
import type { ChatSessionDirectoryEntry } from "../src/contracts/chat";
import { PostgresStorage, mergeSessionDirectories } from "../src/storage/postgres-storage";

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

describe("PostgresStorage storageConfig", () => {
  test("returns plain JSON with only connection keys", () => {
    const storage = new PostgresStorage({
      url: "postgresql://test@localhost/peri",
      httpEndpoint: "http://127.0.0.1:4444/sql",
      legacyCwd: "/workspace",
      currentCwd: "/workspace/",
    });

    const config = storage.storageConfig;

    expect(Object.getPrototypeOf(config)).toBe(Object.prototype);
    expect(Object.keys(config).sort()).toEqual(["httpEndpoint", "url"]);
    expect(config).toEqual({ url: "postgresql://test@localhost/peri", httpEndpoint: "http://127.0.0.1:4444/sql" });
    expect(config).not.toHaveProperty("legacyCwd");
    expect(config).not.toHaveProperty("currentCwd");
    expect(JSON.parse(JSON.stringify(config))).toEqual(config);
    expect(storage.storageConfig).not.toBe(config);
    expect(storage.storageConfig).toEqual(config);
  });

  test("omits httpEndpoint when undefined", () => {
    const config = new PostgresStorage({ url: "postgresql://test@localhost/peri" }).storageConfig;

    expect(Object.keys(config)).toEqual(["url"]);
    expect(config).toEqual({ url: "postgresql://test@localhost/peri" });
    expect(JSON.stringify(config)).toBe('{"url":"postgresql://test@localhost/peri"}');
  });
});

describe("PostgresStorage session directory", () => {
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
    connector: (config: { url: string; httpEndpoint?: string }): Connection => {
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

describe("PostgresStorage connection lifecycle", () => {
  test("reuses one connection across queries and closes it once", async () => {
    const stub = connectionStub([sessionRow("a")]);
    const storage = new PostgresStorage(
      { url: "postgresql://test@localhost/peri" },
      stub.connector,
    );

    expect(await storage.getSession("a")).toMatchObject({ sessionId: "a" });
    await storage.listSessions("/workspace");
    await storage.listSessions("/workspace/");
    expect(stub.opened).toEqual(["postgresql://test@localhost/peri"]);

    await storage.close();
    await storage.close();
    expect(stub.closes()).toBe(1);
  });

  test("serializes concurrent queries on the single stream", async () => {
    const stub = connectionStub([sessionRow("a")]);
    const storage = new PostgresStorage(
      { url: "postgresql://test@localhost/peri" },
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
    const storage = new PostgresStorage(
      { url: "postgresql://test@localhost/peri" },
      stub.connector,
    );

    const pending = storage.getSession("a");
    const closing = storage.close();
    await expect(pending).resolves.toMatchObject({ sessionId: "a" });
    await closing;
    expect(stub.closes()).toBe(1);
    await expect(storage.getSession("a")).rejects.toThrow(
      "PostgresStorage is closed",
    );
  });

  test("retries the close after a failure while still rejecting new queries", async () => {
    let closes = 0;
    const storage = new PostgresStorage(
      { url: "postgresql://test@localhost/peri" },
      () => ({
        all: async () => [sessionRow("a")],
        close: async () => {
          closes += 1;
          if (closes === 1) throw new Error("close failed");
        },
      }) as unknown as Connection,
    );

    expect(await storage.getSession("a")).toMatchObject({ sessionId: "a" });
    await expect(storage.close()).rejects.toThrow("close failed");
    // 第一次 close 未确认：连接仍保留，但新查询立即拒绝。
    await expect(storage.getSession("a")).rejects.toThrow(
      "PostgresStorage is closed",
    );
    await storage.close();
    expect(closes).toBe(2);
    await storage.close();
    expect(closes).toBe(2);
  });
});

describe("PostgresStorage HTTP transport", () => {
  test("requires an explicit trusted proxy for a self-hosted PostgreSQL DSN", async () => {
    const url = "postgresql://test@db.example.org/peri";
    await expect(new PostgresStorage({ url }).getSession("a")).rejects.toThrow("requires httpEndpoint");
    const insecure = new PostgresStorage({ url, httpEndpoint: "http://proxy.example.org/sql" });
    await expect(insecure.getSession("a")).rejects.toThrow("requires HTTPS");
  });

  test("preserves database errors other than an uninitialized directory", async () => {
    const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
      const { queries } = await request.json();
      expect(request.headers.get("Neon-Batch-Read-Only")).toBe("true");
      expect(queries[0].query).toContain("pg_catalog.current_schema()");
      const id = queries[1].params[0];
      return Response.json({ code: id === "missing" ? "42P01" : "42501" }, { status: 400 });
    } });
    try {
      const storage = new PostgresStorage({
        url: "postgresql://test@localhost/peri",
        httpEndpoint: `http://127.0.0.1:${server.port}/sql`,
      });
      expect(await storage.getSession("missing")).toBeNull();
      await expect(storage.getSession("denied")).rejects.toMatchObject({ code: "42501" });
      await storage.close();
    } finally {
      server.stop(true);
    }
  });
});
