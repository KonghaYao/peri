import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import { AtomicSessionClaims } from "../src/chat/claims";
import { SessionBindingConflictError } from "../src/chat/errors";
import { ChatManager } from "../src/chat/manager";
import type { ChatManagerInternalOptions } from "../src/chat/manager";
import { MemoryKV } from "../src/kv/memory-kv";
import {
    FakeDatabase,
    FakePort,
    directoryEntry,
} from "../tests/chat-fixtures";

interface Harness {
    manager: ChatManager;
    claims: AtomicSessionClaims;
    database: FakeDatabase;
    ports: FakePort[];
}

function harness(options: {
    kv?: MemoryKV;
    path?: string;
    database?: FakeDatabase;
} = {}): Harness {
    const kv = options.kv ?? new MemoryKV();
    const claims = new AtomicSessionClaims(kv);
    const database = options.database ?? new FakeDatabase();
    const ports: FakePort[] = [];
    const managerOptions: ChatManagerInternalOptions = {
        agentConfig: { model: "test" },
        database,
        sessionClaims: claims,
        host: {
            kind: "transport",
            factory: () => {
                throw new Error("ACP transport must not start");
            },
        },
        defaults: options.path ? { path: options.path } : {},
        instanceFactory: () => {
            const port = new FakePort();
            ports.push(port);
            return port;
        },
    };
    return { manager: new ChatManager(managerOptions), claims, database, ports };
}

test("MemoryKV claims once and releases only for its owner", async () => {
    const kv = new MemoryKV();
    const results = await Promise.all([
        kv.claimIfAbsent("session", "first"),
        kv.claimIfAbsent("session", "second"),
    ]);
    expect(results).toEqual([true, false]);
    expect(await kv.releaseIfOwner("session", "second")).toBe(false);
    expect(await kv.releaseIfOwner("session", "first")).toBe(true);
    expect(await kv.claimIfAbsent("session", "second")).toBe(true);
});

describe("ChatManager lifecycle", () => {
    test("declaration is synchronous and starts nothing", () => {
        const { manager, claims, database, ports } = harness();
        const chat = manager.createChat({ path: "/workspace" });

        expect(chat.id).toBeTruthy();
        expect(manager.getChat(chat.id)).toBe(chat);
        expect(chat.sessionId).toBeNull();
        expect(chat.bindingRevision).toBe(0);
        expect(ports).toHaveLength(0);
        expect(claims.size).toBe(0);
        expect(database.getCalls).toHaveLength(0);
        expect("execute" in manager).toBe(false);
    });

    test("listSessions returns directory facts without starting a run instance", async () => {
        const { manager, database, ports } = harness();
        database.seed(directoryEntry("session-1", "/persisted"));

        const sessions = await manager.listSessions({ path: "/persisted" });

        expect(sessions.map((entry) => entry.sessionId)).toEqual([
            "session-1",
        ]);
        expect(database.listCalls).toEqual(["/persisted"]);
        expect(ports).toHaveLength(0);
    });

    test("loading a Session never republishes or dispatches old input", async () => {
        const { manager, database, ports } = harness();
        database.seed(directoryEntry("session-1"));
        const chat = manager.createChat();

        await chat.execute(ChatUserAction.changeSessionTo("session-1"));

        expect(chat.sessionId).toBe("session-1");
        expect(chat.bindingRevision).toBe(1);
        expect(
            ports[0].calls.filter(
                (call) =>
                    call.startsWith("send:") ||
                    call.startsWith("dispatch:"),
            ),
        ).toHaveLength(0);
        expect(ports[0].closeCalls).toBe(0);
    });

    test("new Sessions use the handle path and claim the returned identity", async () => {
        const { manager, claims } = harness();
        const chat = manager.createChat({ path: "/custom" });

        const ready = await chat.execute(
            ChatUserAction.createNewSession({ title: "Untitled" }),
        );

        expect(ready.sessionId).toBe("session-new");
        expect(claims.has("session-new")).toBe(true);
        expect(manager.findChatBySession("session-new")).toBe(chat);
    });

    test("close failure retains the handle and claim; retry succeeds", async () => {
        const { manager, claims, database, ports } = harness();
        database.seed(directoryEntry("session-1"));
        const chat = manager.createChat();
        await chat.execute(ChatUserAction.changeSessionTo("session-1"));
        ports[0].closeError = new Error("termination unconfirmed");

        await expect(manager.closeChat(chat.id)).rejects.toThrow(
            "Chat close is incomplete",
        );
        expect(manager.getChat(chat.id)).toBe(chat);
        expect(manager.findChatBySession("session-1")).toBe(chat);
        expect(claims.has("session-1")).toBe(true);

        delete ports[0].closeError;
        await manager.closeChat(chat.id);
        expect(manager.getChat(chat.id)).toBeUndefined();
        expect(claims.size).toBe(0);
    });

    test("shutdown closes handles after stopping admission", async () => {
        const { manager, database, ports } = harness();
        database.seed(directoryEntry("session-1"));
        const chat = manager.createChat();
        await chat.execute(ChatUserAction.changeSessionTo("session-1"));

        await manager.shutdown();

        expect(() => manager.createChat()).toThrow("shut down");
        expect(manager.getChat(chat.id)).toBeUndefined();
        expect(ports[0].closeCalls).toBe(1);
        await manager.shutdown();
    });
});

describe("Session ownership", () => {
    test("two managers racing for the same Session: exactly one loads it", async () => {
        const kv = new MemoryKV();
        const database = new FakeDatabase();
        database.seed(directoryEntry("session-1"));
        const first = harness({ kv, database });
        const second = harness({ kv, database });
        const firstChat = first.manager.createChat();
        const secondChat = second.manager.createChat();

        const results = await Promise.allSettled([
            firstChat.execute(ChatUserAction.changeSessionTo("session-1")),
            secondChat.execute(ChatUserAction.changeSessionTo("session-1")),
        ]);

        expect(
            results.filter((result) => result.status === "fulfilled"),
        ).toHaveLength(1);
        const loser = results.find(
            (result) => result.status === "rejected",
        ) as PromiseRejectedResult;
        expect(loser.reason).toBeInstanceOf(SessionBindingConflictError);
        expect(
            (loser.reason as SessionBindingConflictError).ownerChatId,
        ).toBeUndefined();
        expect(first.ports.length + second.ports.length).toBe(1);
    });

    test("different handle paths cannot bypass the Session claim", async () => {
        const kv = new MemoryKV();
        const database = new FakeDatabase();
        database.seed(directoryEntry("session-1", "/persisted"));
        const first = harness({ kv, database, path: "/first" });
        const second = harness({ kv, database, path: "/second" });
        const firstChat = first.manager.createChat({ path: "/first" });
        const secondChat = second.manager.createChat({ path: "/second" });

        const results = await Promise.allSettled([
            firstChat.execute(ChatUserAction.changeSessionTo("session-1")),
            secondChat.execute(ChatUserAction.changeSessionTo("session-1")),
        ]);

        expect(
            results.filter((result) => result.status === "fulfilled"),
        ).toHaveLength(1);
        expect(
            results.find((result) => result.status === "rejected")?.reason,
        ).toBeInstanceOf(SessionBindingConflictError);
        expect(first.claims.has("session-1")).toBe(
            firstChat.sessionId === "session-1",
        );
        expect(second.claims.has("session-1")).toBe(
            secondChat.sessionId === "session-1",
        );
    });
});
