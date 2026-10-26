import { describe, expect, test } from "bun:test";
import {
  KvSessionOwnership,
  SessionOwnershipConflictError,
  clearSessionOwnership,
  sessionOwnershipKey,
} from "../src/chat/ownership";
import type { SessionOwnershipStorage } from "../src/chat/types";
import { OwnershipProbe } from "../tests/ownership-probe";

// 读后写不是原子操作：两个 Manager 并发 acquire 可能都成功。SDK 不检测该
// 竞态；同一 Session 的单实例绑定由路由层串行化保证。因此这里只测试顺序
// 冲突与交接，不测试“并发 acquire 恰好一个成功”。

describe("session ownership key", () => {
  test("base64url encodes only the Session id", () => {
    expect(sessionOwnershipKey("session:1")).toBe("ownership:c2Vzc2lvbjox");
    expect(sessionOwnershipKey("../x")).toBe("ownership:Li4veA");
    const encoded = (sessionId: string) =>
      sessionOwnershipKey(sessionId).slice("ownership:".length);
    expect(encoded("session:1")).not.toMatch(/[/+=]/u);
    expect(encoded("../x")).not.toMatch(/[/+=]/u);
  });

  test("Unicode Session ids stay distinct and URL-safe", () => {
    const first = sessionOwnershipKey("会话:一");
    const second = sessionOwnershipKey("会话:二");
    expect(first).toMatch(/^ownership:[A-Za-z0-9_-]+$/u);
    expect(second).toMatch(/^ownership:[A-Za-z0-9_-]+$/u);
    expect(first).not.toBe(second);
  });
});

describe("KvSessionOwnership acquire", () => {
  test("writes a record with the Session id and an internal owner", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);

    await ownership.acquire("session-1");

    expect(storage.value("session-1")).toEqual({
      sessionId: "session-1",
      owner: expect.any(String),
    });
    expect(storage.size).toBe(1);
  });

  test("repeated acquisition by the same instance is idempotent", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);
    await ownership.acquire("session-1");
    const record = storage.value("session-1") as { owner: string };

    await ownership.acquire("session-1");
    // 模拟存储回显同一 owner 的写后读重试：仍是同一条记录。
    storage.put("session-1", { sessionId: "session-1", owner: record.owner });
    await ownership.acquire("session-1");

    expect(storage.value("session-1")).toEqual({
      sessionId: "session-1",
      owner: record.owner,
    });
    expect(storage.size).toBe(1);
  });

  test("a different instance conflicts and does not overwrite the record", async () => {
    const storage = new OwnershipProbe();
    const first = new KvSessionOwnership(storage);
    await first.acquire("session-1");
    const original = storage.value("session-1");
    const second = new KvSessionOwnership(storage);

    await expect(second.acquire("session-1")).rejects.toBeInstanceOf(
      SessionOwnershipConflictError,
    );
    expect(storage.value("session-1")).toEqual(original);
  });

  test("different Sessions do not conflict", async () => {
    const storage = new OwnershipProbe();
    const first = new KvSessionOwnership(storage);
    const second = new KvSessionOwnership(storage);

    await first.acquire("session-1");
    await second.acquire("session-2");

    expect(storage.size).toBe(2);
  });

  test("empty Session ids are rejected", async () => {
    const ownership = new KvSessionOwnership(new OwnershipProbe());
    await expect(ownership.acquire("")).rejects.toBeInstanceOf(TypeError);
  });

  test("read failure fails closed without writing", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);
    storage.failRead("session-1");

    await expect(ownership.acquire("session-1")).rejects.toThrow(
      "ownership read failed",
    );
    expect(storage.has("session-1")).toBe(false);

    storage.clearFailure("session-1");
    await ownership.acquire("session-1");
    expect(storage.has("session-1")).toBe(true);
  });

  test("write failure does not mark the Session as owned", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);
    storage.failWrite("session-1");

    await expect(ownership.acquire("session-1")).rejects.toThrow(
      "ownership write failed",
    );
    expect(storage.has("session-1")).toBe(false);
    await expect(ownership.release("session-1")).rejects.toThrow("not held");

    storage.clearFailure("session-1");
    await ownership.acquire("session-1");
    expect(storage.has("session-1")).toBe(true);
  });

  test("malformed records fail closed instead of being treated as empty", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);

    storage.put("session-string", "garbage");
    await expect(ownership.acquire("session-string")).rejects.toBeInstanceOf(
      TypeError,
    );

    storage.put("session-missing-owner", { sessionId: "session-missing-owner" });
    await expect(ownership.acquire("session-missing-owner")).rejects.toBeInstanceOf(
      TypeError,
    );

    storage.put("session-mismatch", {
      sessionId: "another-session",
      owner: "other-owner",
    });
    await expect(ownership.acquire("session-mismatch")).rejects.toBeInstanceOf(
      TypeError,
    );

    // 非法记录不会被覆盖。
    expect(storage.value("session-mismatch")).toEqual({
      sessionId: "another-session",
      owner: "other-owner",
    });
  });
});

describe("KvSessionOwnership release", () => {
  test("removes the record when the owner matches", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);
    await ownership.acquire("session-1");

    await ownership.release("session-1");

    expect(storage.has("session-1")).toBe(false);
    expect(storage.size).toBe(0);
  });

  test("missing records are a no-op and clear local state", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);
    await ownership.acquire("session-1");
    storage.drop("session-1");

    await ownership.release("session-1");
    expect(storage.has("session-1")).toBe(false);
    await expect(ownership.release("session-1")).rejects.toThrow("not held");
  });

  test("records owned by another instance are not removed and clear local state", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);
    await ownership.acquire("session-1");
    const replacement = { sessionId: "session-1", owner: "new-owner" };
    storage.put("session-1", replacement);

    await ownership.release("session-1");

    expect(storage.value("session-1")).toEqual(replacement);
    await expect(ownership.release("session-1")).rejects.toThrow("not held");
  });

  test("releasing a Session that was never acquired throws", async () => {
    const ownership = new KvSessionOwnership(new OwnershipProbe());
    await expect(ownership.release("session-1")).rejects.toThrow("not held");
  });

  test("read failure preserves the record and local ownership for retry", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);
    await ownership.acquire("session-1");
    storage.failRead("session-1");

    await expect(ownership.release("session-1")).rejects.toThrow(
      "ownership read failed",
    );
    expect(storage.has("session-1")).toBe(true);

    storage.clearFailure("session-1");
    await ownership.release("session-1");
    expect(storage.has("session-1")).toBe(false);
  });

  test("remove failure preserves the record and local ownership for retry", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);
    await ownership.acquire("session-1");
    storage.failRelease("session-1");

    await expect(ownership.release("session-1")).rejects.toThrow(
      "ownership release failed",
    );
    expect(storage.has("session-1")).toBe(true);

    storage.clearReleaseFailure("session-1");
    await ownership.release("session-1");
    expect(storage.has("session-1")).toBe(false);
  });

  test("two instances hand over a Session sequentially", async () => {
    const storage = new OwnershipProbe();
    const first = new KvSessionOwnership(storage);
    const second = new KvSessionOwnership(storage);

    await first.acquire("session-1");
    await expect(second.acquire("session-1")).rejects.toBeInstanceOf(
      SessionOwnershipConflictError,
    );
    await first.release("session-1");
    await second.acquire("session-1");
    await expect(first.acquire("session-1")).rejects.toBeInstanceOf(
      SessionOwnershipConflictError,
    );
    await second.release("session-1");
    expect(storage.size).toBe(0);
  });
});

describe("ambiguous storage outcomes", () => {
  test("retry after a lost write response is idempotent for the same owner", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);
    storage.failWriteAfterWrite("session-1");

    await expect(ownership.acquire("session-1")).rejects.toThrow(
      "ownership write response lost",
    );
    expect(storage.has("session-1")).toBe(true);

    storage.clearFailure("session-1");
    // 记录已由本实例写入，但本地 owned 尚未登记；重试读取同 owner 后幂等返回。
    await ownership.acquire("session-1");
    expect(storage.size).toBe(1);

    await ownership.release("session-1");
    expect(storage.has("session-1")).toBe(false);
  });

  test("release rejects a malformed record and retains local ownership", async () => {
    const storage = new OwnershipProbe();
    const ownership = new KvSessionOwnership(storage);
    await ownership.acquire("session-1");
    storage.put("session-1", "garbage");

    await expect(ownership.release("session-1")).rejects.toBeInstanceOf(
      TypeError,
    );
    expect(storage.has("session-1")).toBe(true);
    // 本地 owned 未被清除：再次 release 仍读取记录，而不是报 "not held"。
    await expect(ownership.release("session-1")).rejects.toBeInstanceOf(
      TypeError,
    );
  });
});

describe("clearSessionOwnership", () => {
  test("removes only the target Session without reading the record", async () => {
    const storage = new OwnershipProbe();
    storage.hold("session-1");
    storage.hold("session-2");
    storage.failRead("session-1");

    await clearSessionOwnership(storage, "session-1");

    expect(storage.has("session-1")).toBe(false);
    expect(storage.has("session-2")).toBe(true);
  });

  test("rejects empty Session ids", async () => {
    const storage = new OwnershipProbe();
    await expect(clearSessionOwnership(storage, "")).rejects.toBeInstanceOf(
      TypeError,
    );
  });

  test("propagates remove failures", async () => {
    const storage = new OwnershipProbe();
    storage.hold("session-1");
    storage.failRelease("session-1");

    await expect(clearSessionOwnership(storage, "session-1")).rejects.toThrow(
      "ownership release failed",
    );
    expect(storage.has("session-1")).toBe(true);
  });

  test("requires a storage with removeItem", async () => {
    await expect(
      clearSessionOwnership({} as SessionOwnershipStorage, "session-1"),
    ).rejects.toBeInstanceOf(TypeError);
  });
});

describe("KvSessionOwnership storage validation", () => {
  test("rejects objects without the 3-method port", () => {
    expect(
      () => new KvSessionOwnership({} as SessionOwnershipStorage),
    ).toThrow(TypeError);
  });
});
