import { describe, expect, test } from "bun:test";
import { AtomicSessionClaims } from "../src/chat/claims";
import { SessionClaimConflictError } from "../src/kv/session-claim-conflict-error";
import { sessionClaimKey } from "../src/kv/session-claim-key";
import { MemoryKV } from "../src/kv/memory-kv";

describe("Session claim coordination domain", () => {
  test("different managers racing for the same Session have exactly one owner", async () => {
    const kv = new MemoryKV();
    const first = new AtomicSessionClaims(kv);
    const second = new AtomicSessionClaims(kv);

    const results = await Promise.allSettled([
      first.claim("session-1"),
      second.claim("session-1"),
    ]);

    expect(results.filter((result) => result.status === "fulfilled")).toHaveLength(1);
    const rejected = results.find((result) => result.status === "rejected") as PromiseRejectedResult;
    expect(rejected.reason).toBeInstanceOf(SessionClaimConflictError);
    expect(rejected.reason.key).toBe(sessionClaimKey("session-1"));
    expect(rejected.reason.sessionId).toBe("session-1");
    await Promise.all([first.releaseAll(), second.releaseAll()]);
  });

  test("different Sessions in different managers do not conflict", async () => {
    const kv = new MemoryKV();
    const first = new AtomicSessionClaims(kv);
    const second = new AtomicSessionClaims(kv);

    await Promise.all([first.claim("session-1"), second.claim("session-2")]);

    await Promise.all([first.releaseAll(), second.releaseAll()]);
  });

  test("a Session can be taken over only after its original owner releases it", async () => {
    const kv = new MemoryKV();
    const original = new AtomicSessionClaims(kv);
    const contender = new AtomicSessionClaims(kv);
    await original.claim("session-1");

    await expect(contender.claim("session-1")).rejects.toBeInstanceOf(SessionClaimConflictError);
    expect(await kv.releaseIfOwner(sessionClaimKey("session-1"), "another-owner")).toBe(false);

    await original.release("session-1");
    await contender.claim("session-1");
    await expect(original.claim("session-1")).rejects.toBeInstanceOf(SessionClaimConflictError);
    await contender.releaseAll();
  });

  test("same-instance repeated claims are idempotent and release is owner-checked", async () => {
    const claims = new AtomicSessionClaims(new MemoryKV());
    await claims.claim("session-1");
    await claims.claim("session-1");
    expect(claims.size).toBe(1);
    expect(claims.has("session-1")).toBe(true);

    await claims.release("session-1");
    expect(claims.size).toBe(0);
    await expect(claims.release("session-1")).rejects.toThrow("not owned");
  });

  test("Session keys are scoped only by Session id", async () => {
    expect(sessionClaimKey("session:1")).toBe("peri:managed-agent:session:session%3A1");
    const kv = new MemoryKV();
    const first = new AtomicSessionClaims(kv);
    const second = new AtomicSessionClaims(kv);
    await first.claim("session-1");
    await expect(second.claim("session-1")).rejects.toBeInstanceOf(SessionClaimConflictError);
    await first.releaseAll();
  });
});
