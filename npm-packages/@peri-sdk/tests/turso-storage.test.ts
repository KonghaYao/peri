import { describe, expect, test } from "bun:test";
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
