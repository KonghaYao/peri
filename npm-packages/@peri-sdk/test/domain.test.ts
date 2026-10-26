import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import { SessionBindingConflictError } from "../src/chat/errors";
import { ChatManager } from "../src/chat/manager";
import type { ChatManagerInternalOptions } from "../src/chat/manager";
import {
    FakeDatabase,
    FakePort,
    directoryEntry,
} from "../tests/chat-fixtures";
import { OwnershipProbe } from "../tests/ownership-probe";

interface Harness {
    manager: ChatManager;
    ownership: OwnershipProbe;
    database: FakeDatabase;
    ports: FakePort[];
}

function harness(options: {
    ownership?: OwnershipProbe;
    path?: string;
    database?: FakeDatabase;
} = {}): Harness {
    const ownership = options.ownership ?? new OwnershipProbe();
    const database = options.database ?? new FakeDatabase();
    const ports: FakePort[] = [];
    const managerOptions: ChatManagerInternalOptions = {
        agentConfig: { model: "test" },
        database,
        sessionOwnershipStorage: ownership,
        chatConfig: {
            runtime: {
                kind: "transport",
                factory: () => {
                    throw new Error("ACP transport must not start");
                },
            },
            defaults: options.path ? { path: options.path } : {},
        },
        instanceFactory: () => {
            const port = new FakePort();
            ports.push(port);
            return port;
        },
    };
    return { manager: new ChatManager(managerOptions), ownership, database, ports };
}

describe("ChatManager lifecycle", () => {
    test("declaration is synchronous and starts nothing", () => {
        const { manager, ownership, database, ports } = harness();
        const chat = manager.createChat({ path: "/workspace" });

        expect(chat.id).toBeTruthy();
        expect(manager.getChat(chat.id)).toBe(chat);
        expect(chat.sessionId).toBeNull();
        expect(chat.bindingRevision).toBe(0);
        expect(ports).toHaveLength(0);
        expect(ownership.size).toBe(0);
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

    test("new Sessions use the handle path and acquire the returned identity", async () => {
        const { manager, ownership } = harness();
        const chat = manager.createChat({ path: "/custom" });

        const ready = await chat.execute(
            ChatUserAction.createNewSession({ title: "Untitled" }),
        );

        expect(ready.sessionId).toBe("session-new");
        expect(ownership.has("session-new")).toBe(true);
        expect(manager.findChatBySession("session-new")).toBe(chat);
    });

    test("close failure retains the handle and ownership; retry succeeds", async () => {
        const { manager, ownership, database, ports } = harness();
        database.seed(directoryEntry("session-1"));
        const chat = manager.createChat();
        await chat.execute(ChatUserAction.changeSessionTo("session-1"));
        ports[0].closeError = new Error("termination unconfirmed");

        await expect(manager.closeChat(chat.id)).rejects.toThrow(
            "Chat close is incomplete",
        );
        expect(manager.getChat(chat.id)).toBe(chat);
        expect(manager.findChatBySession("session-1")).toBe(chat);
        expect(ownership.has("session-1")).toBe(true);

        delete ports[0].closeError;
        await manager.closeChat(chat.id);
        expect(manager.getChat(chat.id)).toBeUndefined();
        expect(ownership.size).toBe(0);
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
    // 读后写非原子：跨 Manager 的并发 acquire 不由 SDK 串行化；路由层负责
    // 单实例绑定。这里验证顺序冲突。
    test("a second manager cannot load a Session already held by another manager", async () => {
        const ownership = new OwnershipProbe();
        const database = new FakeDatabase();
        database.seed(directoryEntry("session-1"));
        const first = harness({ ownership, database });
        const second = harness({ ownership, database });
        const firstChat = first.manager.createChat();
        const secondChat = second.manager.createChat();

        await firstChat.execute(
            ChatUserAction.changeSessionTo("session-1"),
        );
        await expect(
            secondChat.execute(
                ChatUserAction.changeSessionTo("session-1"),
            ),
        ).rejects.toBeInstanceOf(SessionBindingConflictError);

        expect(firstChat.sessionId).toBe("session-1");
        expect(secondChat.sessionId).toBeNull();
        expect(first.ports.length + second.ports.length).toBe(1);
    });

    test("different handle paths cannot bypass SessionOwnership", async () => {
        const ownership = new OwnershipProbe();
        const database = new FakeDatabase();
        database.seed(directoryEntry("session-1", "/persisted"));
        const first = harness({ ownership, database, path: "/first" });
        const second = harness({ ownership, database, path: "/second" });
        const firstChat = first.manager.createChat({ path: "/first" });
        const secondChat = second.manager.createChat({ path: "/second" });

        await firstChat.execute(
            ChatUserAction.changeSessionTo("session-1"),
        );
        await expect(
            secondChat.execute(
                ChatUserAction.changeSessionTo("session-1"),
            ),
        ).rejects.toBeInstanceOf(SessionBindingConflictError);

        expect(first.ownership.has("session-1")).toBe(true);
        expect(first.ownership.size).toBe(1);
        expect(
            [firstChat.sessionId, secondChat.sessionId].filter(
                (sessionId) => sessionId === "session-1",
            ),
        ).toEqual(["session-1"]);
    });
});
