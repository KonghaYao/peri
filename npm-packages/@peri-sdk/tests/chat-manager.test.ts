import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import {
    SessionBindingConflictError,
} from "../src/chat/errors";
import { ChatManager } from "../src/chat/manager";
import type { ChatManagerInternalOptions } from "../src/chat/manager";
import {
    FakeDatabase,
    FakePort,
    directoryEntry,
} from "./chat-fixtures";
import { OwnershipProbe } from "./ownership-probe";

function setup(options: {
    ownership?: OwnershipProbe;
    database?: FakeDatabase;
    configure?: (port: FakePort) => void;
} = {}): {
    manager: ChatManager;
    ownership: OwnershipProbe;
    database: FakeDatabase;
    ports: FakePort[];
} {
    const ownership = options.ownership ?? new OwnershipProbe();
    const database = options.database ?? new FakeDatabase();
    const ports: FakePort[] = [];
    const managerOptions: ChatManagerInternalOptions = {
        agentConfig: {},
        database,
        sessionOwnershipStorage: ownership,
        chatConfig: {
            runtime: {
                kind: "transport",
                factory: () => {
                    throw new Error("ACP transport must not start");
                },
            },
            defaults: { path: "/workspace" },
        },
        instanceFactory: () => {
            const port = new FakePort();
            options.configure?.(port);
            ports.push(port);
            return port;
        },
    };
    return {
        manager: new ChatManager(managerOptions),
        ownership,
        database,
        ports,
    };
}

async function bindNew(
    chat: ReturnType<ChatManager["createChat"]>,
    requestId = "create-1",
): Promise<void> {
    await chat.execute(
        ChatUserAction.createNewSession({ requestId }),
    );
}

describe("ChatManager handle directory", () => {
    test("createChat is synchronous and starts no ACP, ownership acquisition or session", () => {
        const { manager, ownership, database, ports } = setup();
        const chat = manager.createChat({ path: "/custom" });

        expect(chat.id).toBeTruthy();
        expect(manager.getChat(chat.id)).toBe(chat);
        expect(manager.findChatBySession("missing")).toBeUndefined();
        expect(ports).toHaveLength(0);
        expect(ownership.size).toBe(0);
        expect(database.getCalls).toHaveLength(0);
        expect("execute" in manager).toBe(false);
    });

    test("create_new_session binds, acquires ownership and indexes without a prefilled session id", async () => {
        const { manager, ownership, ports } = setup();
        const chat = manager.createChat();
        const action = ChatUserAction.createNewSession({ requestId: "req-1" });

        const result = await chat.execute(action);

        expect(result).toEqual({
            chatId: chat.id,
            sessionId: "session-new",
            bindingRevision: 1,
            requestId: "req-1",
        });
        expect(chat.sessionId).toBe("session-new");
        expect(chat.bindingRevision).toBe(1);
        expect(ports[0].calls).toContain("bindNew");
        expect(ownership.has("session-new")).toBe(true);
        expect(manager.findChatBySession("session-new")).toBe(chat);
    });

    test("partial creation surfaces createdSessionId and stage without retrying", async () => {
        const { manager, ownership } = setup({
            configure: (port) => {
                port.bindNewError = Object.assign(
                    new Error("rename failed"),
                    { createdSessionId: "session-partial", stage: "rename" },
                );
            },
        });
        const chat = manager.createChat();

        await expect(
            chat.execute(
                ChatUserAction.createNewSession({ requestId: "create-1" }),
            ),
        ).rejects.toMatchObject({
            requestId: "create-1",
            code: "session_creation_failed",
            createdSessionId: "session-partial",
            stage: "rename",
            disposition: "unknown",
        });
        expect(chat.phase).toBe("empty");
        expect(ownership.has("session-partial")).toBe(false);
    });

    test("getChat is stable for the manager lifetime", () => {
        const { manager } = setup();
        const first = manager.createChat();
        const second = manager.createChat();
        expect(manager.getChat(first.id)).toBe(first);
        expect(manager.getChat(second.id)).toBe(second);
        expect(manager.getChat(first.id)).toBe(first);
    });

    test("listSessions forwards path and never starts a run instance", async () => {
        const { manager, database, ports } = setup();
        database.seed(directoryEntry("session-1", "/persisted"));
        expect(await manager.listSessions({ path: "/scope" })).toHaveLength(1);
        expect(await manager.listSessions()).toHaveLength(1);
        expect(database.listCalls).toEqual(["/scope", undefined]);
        expect(ports).toHaveLength(0);

        database.listError = new Error("database unavailable");
        await expect(manager.listSessions()).rejects.toThrow(
            "database unavailable",
        );
    });

    test("findChatBySession covers binding, bound and cleaning", async () => {
        let release!: () => void;
        const gate = new Promise<void>((resolve) => {
            release = resolve;
        });
        const { manager, ownership, database } = setup({
            configure: (port) => {
                port.bindExistingGate = gate;
            },
        });
        database.seed(directoryEntry("session-1"));
        const chat = manager.createChat();
        const loading = chat.execute(
            ChatUserAction.changeSessionTo("session-1", {
                requestId: "load-1",
            }),
        );

        await waitFor(() => manager.findChatBySession("session-1") === chat);
        expect(chat.phase).toBe("switching");
        // 索引先于 path resolve/ownership 写入；acquire 在其后结算。
        await waitFor(() => ownership.has("session-1"));

        release();
        await loading;
        expect(manager.findChatBySession("session-1")).toBe(chat);
        expect(chat.phase).toBe("ready");
    });

    test("failed binding removes the index only after confirmed cleanup", async () => {
        const database = new FakeDatabase();
        database.seed(directoryEntry("session-1"));
        const { manager, ownership } = setup({
            database,
            configure: (port) => {
                port.bindExistingError = new Error("load failed");
            },
        });
        const chat = manager.createChat();
        await expect(
            chat.execute(ChatUserAction.changeSessionTo("session-1")),
        ).rejects.toThrow("load failed");
        expect(chat.binding).toBeNull();
        expect(chat.phase).toBe("empty");
        expect(manager.findChatBySession("session-1")).toBeUndefined();
        expect(ownership.size).toBe(0);
    });

    test("unconfirmed binding cleanup retains the index and ownership for retry", async () => {
        const database = new FakeDatabase();
        database.seed(directoryEntry("session-1"));
        const { manager, ownership, ports } = setup({
            database,
            configure: (port) => {
                port.bindExistingError = new Error("load failed");
                port.closeError = new Error("cleanup failed");
            },
        });
        const chat = manager.createChat();

        await expect(
            chat.execute(ChatUserAction.changeSessionTo("session-1")),
        ).rejects.toMatchObject({
            code: "binding_failed",
            disposition: "unknown",
        });
        expect(chat.phase).toBe("cleanup-pending");
        expect(manager.findChatBySession("session-1")).toBe(chat);
        expect(ownership.has("session-1")).toBe(true);

        delete ports[0].closeError;
        await manager.closeChat(chat.id);
        expect(manager.getChat(chat.id)).toBeUndefined();
        expect(manager.findChatBySession("session-1")).toBeUndefined();
        expect(ownership.size).toBe(0);
    });
});

describe("ChatManager ownership coordination", () => {
    test("same manager reports the owning chat id for a duplicate session load", async () => {
        let release!: () => void;
        const gate = new Promise<void>((resolve) => {
            release = resolve;
        });
        const { manager, database } = setup({
            configure: (port) => {
                port.bindExistingGate = gate;
            },
        });
        database.seed(directoryEntry("session-1"));
        const owner = manager.createChat();
        const loading = owner.execute(
            ChatUserAction.changeSessionTo("session-1"),
        );
        await waitFor(() => manager.findChatBySession("session-1") === owner);

        const contender = manager.createChat();
        let conflict: unknown;
        try {
            await contender.execute(
                ChatUserAction.changeSessionTo("session-1", {
                    requestId: "contender-1",
                }),
            );
        } catch (error) {
            conflict = error;
        }
        expect(conflict).toBeInstanceOf(SessionBindingConflictError);
        expect((conflict as SessionBindingConflictError).ownerChatId).toBe(
            owner.id,
        );
        expect((conflict as SessionBindingConflictError).requestId).toBe(
            "contender-1",
        );

        release();
        await loading;
    });

    test("a second manager cannot acquire a Session already held by another manager", async () => {
        const ownership = new OwnershipProbe();
        const database = new FakeDatabase();
        database.seed(directoryEntry("session-1"));
        const first = setup({ ownership, database });
        const second = setup({ ownership, database });
        const firstChat = first.manager.createChat();
        const secondChat = second.manager.createChat();

        await firstChat.execute(
            ChatUserAction.changeSessionTo("session-1"),
        );
        // 读后写不是原子操作；跨 Manager 的并发由路由层串行化。这里验证
        // 第一个 Manager 完成登记后，第二个按顺序 acquire 会冲突。
        await expect(
            secondChat.execute(
                ChatUserAction.changeSessionTo("session-1"),
            ),
        ).rejects.toBeInstanceOf(SessionBindingConflictError);

        expect(firstChat.sessionId).toBe("session-1");
        expect(secondChat.sessionId).toBeNull();
        expect(ownership.has("session-1")).toBe(true);
    });

    test("load-path storage failures fail closed with claim_failed and allow retry", async () => {
        for (const failure of ["failRead", "failWrite"] as const) {
            const { manager, ownership, database, ports } = setup();
            database.seed(directoryEntry("session-1"));
            ownership[failure]("session-1");
            const chat = manager.createChat();

            await expect(
                chat.execute(
                    ChatUserAction.changeSessionTo("session-1"),
                ),
            ).rejects.toMatchObject({
                code: "claim_failed",
                disposition: "rejected",
            });

            expect(chat.sessionId).toBeNull();
            expect(chat.binding).toBeNull();
            expect(manager.findChatBySession("session-1")).toBeUndefined();
            expect(ownership.has("session-1")).toBe(false);
            expect(ports).toHaveLength(0);

            ownership.clearFailure("session-1");
            const ready = await chat.execute(
                ChatUserAction.changeSessionTo("session-1"),
            );
            expect(ready.sessionId).toBe("session-1");
            expect(ownership.has("session-1")).toBe(true);
        }
    });
});

describe("ChatManager close and shutdown", () => {
    test("closeChat is idempotent for unknown ids and removes only confirmed closes", async () => {
        const { manager, ownership, database } = setup();
        await manager.closeChat("unknown");
        database.seed(directoryEntry("session-1"));
        const chat = manager.createChat();
        await chat.execute(ChatUserAction.changeSessionTo("session-1"));

        await manager.closeChat(chat.id);

        expect(manager.getChat(chat.id)).toBeUndefined();
        expect(manager.findChatBySession("session-1")).toBeUndefined();
        expect(ownership.size).toBe(0);
        await manager.closeChat(chat.id);
    });

    test("close failure retains the handle, index and ownership; retry succeeds", async () => {
        const { manager, ownership, database, ports } = setup();
        database.seed(directoryEntry("session-1"));
        const chat = manager.createChat();
        await chat.execute(ChatUserAction.changeSessionTo("session-1"));
        ports[0].closeError = new Error("close unconfirmed");

        await expect(manager.closeChat(chat.id)).rejects.toThrow(
            "Chat close is incomplete",
        );
        expect(manager.getChat(chat.id)).toBe(chat);
        expect(manager.findChatBySession("session-1")).toBe(chat);
        expect(ownership.has("session-1")).toBe(true);
        expect(chat.phase).toBe("cleanup-pending");

        delete ports[0].closeError;
        await manager.closeChat(chat.id);
        expect(manager.getChat(chat.id)).toBeUndefined();
        expect(ownership.size).toBe(0);
        expect(ports[0].closeCalls).toBe(2);
    });

    test("shutdown stops admission, aggregates failures and can retry", async () => {
        const { manager, ownership, database, ports } = setup();
        database.seed(directoryEntry("session-1"));
        database.seed(directoryEntry("session-2"));
        const first = manager.createChat();
        const second = manager.createChat();
        await first.execute(ChatUserAction.changeSessionTo("session-1"));
        await second.execute(ChatUserAction.changeSessionTo("session-2"));
        ports[0].closeError = new Error("close unconfirmed");

        await expect(manager.shutdown()).rejects.toThrow(
            "ChatManager shutdown is incomplete",
        );
        expect(() => manager.createChat()).toThrow("shut down");
        expect(manager.getChat(first.id)).toBe(first);
        expect(manager.getChat(second.id)).toBeUndefined();
        expect(manager.findChatBySession("session-1")).toBe(first);

        delete ports[0].closeError;
        await manager.shutdown();
        expect(manager.getChat(first.id)).toBeUndefined();
        expect(manager.findChatBySession("session-1")).toBeUndefined();
        expect(ownership.size).toBe(0);
        await manager.shutdown();
    });
});

async function waitFor(predicate: () => boolean): Promise<void> {
    for (let attempt = 0; attempt < 100; attempt += 1) {
        if (predicate()) return;
        await Promise.resolve();
    }
    throw new Error("condition was not reached");
}
