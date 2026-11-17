import { expect, test } from "bun:test";
import { readSessionOwnership } from "../src/chat/ownership";
import { OwnershipProbe } from "./ownership-probe";

const SESSION = "session_owner_read";

test("reads an absent ownership record as null", async () => {
    const storage = new OwnershipProbe();
    expect(await readSessionOwnership(storage, SESSION)).toBeNull();
});

test("reads the owner without changing the record", async () => {
    const storage = new OwnershipProbe();
    await storage.setItem(`ownership:${Buffer.from(SESSION).toString("base64url")}`, {
        sessionId: SESSION,
        owner: "owner_1",
    });

    expect(await readSessionOwnership(storage, SESSION)).toEqual({
        sessionId: SESSION,
        owner: "owner_1",
    });
    expect(storage.size).toBe(1);
});

test("fails closed on a malformed record", async () => {
    const storage = new OwnershipProbe();
    await storage.setItem(`ownership:${Buffer.from(SESSION).toString("base64url")}`, {
        sessionId: SESSION,
    });

    await expect(readSessionOwnership(storage, SESSION)).rejects.toThrow(
        /missing sessionId\/owner/u,
    );
});

test("propagates storage read failures", async () => {
    const storage = new OwnershipProbe();
    storage.failRead(SESSION, new Error("kv unavailable"));

    await expect(readSessionOwnership(storage, SESSION)).rejects.toThrow(
        "kv unavailable",
    );
});
