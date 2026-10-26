import { describe, expect, test } from "bun:test";
import { AtomicSessionOwnership } from "../src/chat/ownership";
import { SessionOwnershipConflictError } from "../src/kv/session-ownership-conflict-error";
import { sessionOwnershipKey } from "../src/kv/session-ownership-key";
import { MemoryKV } from "../src/kv/memory-kv";

describe("SessionOwnership coordination domain", () => {
  test("different managers racing for the same Session have exactly one owner", async () => {
    const kv = new MemoryKV();
    const first = new AtomicSessionOwnership(kv);
    const second = new AtomicSessionOwnership(kv);

    const results = await Promise.allSettled([
      first.acquire("session-1"),
      second.acquire("session-1"),
    ]);

    expect(results.filter((result) => result.status === "fulfilled")).toHaveLength(1);
    const rejected = results.find((result) => result.status === "rejected") as PromiseRejectedResult;
    expect(rejected.reason).toBeInstanceOf(SessionOwnershipConflictError);
    expect(rejected.reason.key).toBe(sessionOwnershipKey("session-1"));
    expect(rejected.reason.sessionId).toBe("session-1");
    await Promise.all([first.releaseAll(), second.releaseAll()]);
  });

  test("different Sessions in different managers do not conflict", async () => {
    const kv = new MemoryKV();
    const first = new AtomicSessionOwnership(kv);
    const second = new AtomicSessionOwnership(kv);

    await Promise.all([first.acquire("session-1"), second.acquire("session-2")]);

    await Promise.all([first.releaseAll(), second.releaseAll()]);
  });

  test("a Session can be taken over only after its original owner releases it", async () => {
    const kv = new MemoryKV();
    const original = new AtomicSessionOwnership(kv);
    const contender = new AtomicSessionOwnership(kv);
    await original.acquire("session-1");

    await expect(contender.acquire("session-1")).rejects.toBeInstanceOf(SessionOwnershipConflictError);
    expect(await kv.releaseIfOwner(sessionOwnershipKey("session-1"), "another-owner")).toBe(false);

    await original.release("session-1");
    await contender.acquire("session-1");
    await expect(original.acquire("session-1")).rejects.toBeInstanceOf(SessionOwnershipConflictError);
    await contender.releaseAll();
  });

  test("same-instance repeated acquisitions are idempotent and release is owner-checked", async () => {
    const ownership = new AtomicSessionOwnership(new MemoryKV());
    await ownership.acquire("session-1");
    await ownership.acquire("session-1");
    expect(ownership.size).toBe(1);
    expect(ownership.has("session-1")).toBe(true);

    await ownership.release("session-1");
    expect(ownership.size).toBe(0);
    await expect(ownership.release("session-1")).rejects.toThrow("not held");
  });

  test("Session keys are scoped only by Session id", async () => {
    expect(sessionOwnershipKey("session:1")).toBe("peri:managed-agent:session:session%3A1");
    const kv = new MemoryKV();
    const first = new AtomicSessionOwnership(kv);
    const second = new AtomicSessionOwnership(kv);
    await first.acquire("session-1");
    await expect(second.acquire("session-1")).rejects.toBeInstanceOf(SessionOwnershipConflictError);
    await first.releaseAll();
  });
});
