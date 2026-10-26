import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import { AtomicSessionOwnership } from "../src/chat/ownership";
import {
    ChatClosedError,
    ChatCloseIncompleteError,
    SessionCreationError,
} from "../src/chat/errors";
import { ChatManager } from "../src/chat/manager";
import type { ChatManagerInternalOptions } from "../src/chat/manager";
import type { ChatFactObserver } from "../src/chat/facts";
import type { ChatRuntime, ChatLifecycleOptions } from "../src/chat/types";
import { MemoryKV } from "../src/kv/memory-kv";
import { DeadlineError } from "../src/util/deadline";
import {
    FakeDatabase,
    FakePort,
    directoryEntry,
} from "./chat-fixtures";

interface Setup {
    readonly manager: ChatManager;
    readonly ownership: AtomicSessionOwnership;
    readonly database: FakeDatabase;
    readonly ports: FakePort[];
}

function setup(
    configure?: (port: FakePort) => void,
    lifecycle?: ChatLifecycleOptions,
    runtime?: ChatRuntime,
): Setup {
    const kv = new MemoryKV();
    const database = new FakeDatabase();
    const ports: FakePort[] = [];
    const ownership = new AtomicSessionOwnership(kv);
    const options: ChatManagerInternalOptions = {
        agentConfig: {},
        database,
        sessionOwnership: ownership,
        runtime: runtime ?? {
            kind: "transport",
            factory: () => {
                throw new Error("ACP transport must not start");
            },
        },
        defaults: { path: "/workspace" },
        lifecycle,
        instanceFactory: () => {
            const port = new FakePort();
            configure?.(port);
            ports.push(port);
            return port;
        },
    };
    return { manager: new ChatManager(options), ownership, database, ports };
}

function deferred(): {
    readonly promise: Promise<void>;
    resolve(): void;
} {
    let resolve!: () => void;
    const promise = new Promise<void>((onResolve) => {
        resolve = onResolve;
    });
    return { promise, resolve };
}

async function waitFor(predicate: () => boolean): Promise<void> {
    for (let attempt = 0; attempt < 1000; attempt += 1) {
        if (predicate()) return;
        await new Promise<void>((resolve) => setTimeout(resolve, 1));
    }
    throw new Error("condition was not reached");
}

function recorder(): { observer: ChatFactObserver; closed: () => boolean } {
    let closed = false;
    return {
        observer: {
            onBinding() {},
            onFact() {},
            onClosed() {
                closed = true;
            },
        },
        closed: () => closed,
    };
}

describe("bounded close during startup", () => {
    test("close after session/new but before rename is incomplete within budget and retains ownership/index", async () => {
        const createdGate = deferred();
        const setupValue = setup(
            (port) => {
                port.bindNewCreatedGate = createdGate.promise;
            },
            {
                startupSettleTimeoutMs: 20,
                activationCloseTimeoutMs: 100,
            },
        );
        const chat = setupValue.manager.createChat();
        const recorded = recorder();
        chat.subscribeFacts(recorded.observer);
        const creating = chat.execute(
            ChatUserAction.createNewSession({ requestId: "create-1" }),
        );

        await waitFor(() => setupValue.ownership.has("session-new"));
        expect(chat.phase).toBe("binding");
        expect(setupValue.manager.findChatBySession("session-new")).toBe(chat);

        const started = performance.now();
        let closeError: unknown;
        try {
            await chat.close();
        } catch (error) {
            closeError = error;
        }
        const elapsed = performance.now() - started;

        expect(closeError).toBeInstanceOf(ChatCloseIncompleteError);
        expect((closeError as ChatCloseIncompleteError).failures[0]).toBeInstanceOf(
            DeadlineError,
        );
        expect(elapsed).toBeLessThan(2000);
        expect(chat.phase).toBe("cleanup-pending");
        expect(setupValue.ownership.has("session-new")).toBe(true);
        expect(setupValue.manager.findChatBySession("session-new")).toBe(chat);
        expect(setupValue.manager.getChat(chat.id)).toBe(chat);
        expect(recorded.closed()).toBe(false);

        createdGate.resolve();
        await expect(creating).rejects.toBeInstanceOf(SessionCreationError);
        await waitFor(() => !setupValue.ownership.has("session-new"));

        await chat.close();
        expect(recorded.closed()).toBe(true);
        expect(setupValue.manager.getChat(chat.id)).toBeUndefined();
        expect(
            setupValue.manager.findChatBySession("session-new"),
        ).toBeUndefined();
    });

    test("close during path resolution times out with index retained, before any ownership acquisition", async () => {
        const setupValue = setup(undefined, {
            startupSettleTimeoutMs: 20,
        });
        setupValue.database.seed(directoryEntry("session-1"));
        const originalGetSession = setupValue.database.getSession.bind(
            setupValue.database,
        );
        const pathGate = deferred();
        const pathStarted = deferred();
        setupValue.database.getSession = async (id) => {
            pathStarted.resolve();
            await pathGate.promise;
            return originalGetSession(id);
        };

        const chat = setupValue.manager.createChat();
        const loading = chat.execute(
            ChatUserAction.changeSessionTo("session-1", {
                requestId: "load-1",
            }),
        );
        await pathStarted.promise;
        expect(setupValue.manager.findChatBySession("session-1")).toBe(chat);

        let closeError: unknown;
        try {
            await chat.close();
        } catch (error) {
            closeError = error;
        }
        expect(closeError).toBeInstanceOf(ChatCloseIncompleteError);
        expect(setupValue.ownership.has("session-1")).toBe(false);
        expect(setupValue.manager.findChatBySession("session-1")).toBe(chat);

        pathGate.resolve();
        await expect(loading).rejects.toBeInstanceOf(ChatClosedError);
        await chat.close();
        expect(
            setupValue.manager.findChatBySession("session-1"),
        ).toBeUndefined();
    });

    test("activation close timeout is incomplete and a later close confirms", async () => {
        const setupValue = setup(undefined, {
            startupSettleTimeoutMs: 100,
            activationCloseTimeoutMs: 20,
        });
        setupValue.database.seed(directoryEntry("session-1"));
        const chat = setupValue.manager.createChat();
        await chat.execute(ChatUserAction.changeSessionTo("session-1"));
        const port = setupValue.ports[0];
        const originalClose = port.close.bind(port);
        port.close = () => new Promise<void>(() => {});

        let closeError: unknown;
        try {
            await chat.close();
        } catch (error) {
            closeError = error;
        }
        expect(closeError).toBeInstanceOf(ChatCloseIncompleteError);
        expect((closeError as ChatCloseIncompleteError).failures[0]).toBeInstanceOf(
            DeadlineError,
        );
        expect(chat.phase).toBe("cleanup-pending");
        expect(setupValue.ownership.has("session-1")).toBe(true);
        expect(setupValue.manager.findChatBySession("session-1")).toBe(chat);
        expect(setupValue.manager.getChat(chat.id)).toBe(chat);

        port.close = originalClose;
        await chat.close();
        expect(setupValue.manager.getChat(chat.id)).toBeUndefined();
        expect(setupValue.ownership.size).toBe(0);
        expect(
            setupValue.manager.findChatBySession("session-1"),
        ).toBeUndefined();
    });
});

describe("bounded shutdown", () => {
    test("shutdown stops admission, bounds each close and can retry", async () => {
        const createdGate = deferred();
        const setupValue = setup(
            (port) => {
                port.bindNewCreatedGate = createdGate.promise;
            },
            {
                startupSettleTimeoutMs: 50,
                shutdownChatTimeoutMs: 20,
                activationCloseTimeoutMs: 50,
            },
        );
        const chat = setupValue.manager.createChat();
        const creating = chat.execute(
            ChatUserAction.createNewSession({ requestId: "create-1" }),
        );
        await waitFor(() => setupValue.ownership.has("session-new"));

        const started = performance.now();
        let shutdownError: unknown;
        try {
            await setupValue.manager.shutdown();
        } catch (error) {
            shutdownError = error;
        }
        expect(shutdownError).toBeInstanceOf(AggregateError);
        expect(performance.now() - started).toBeLessThan(2000);
        expect(() => setupValue.manager.createChat()).toThrow("shut down");
        expect(setupValue.manager.getChat(chat.id)).toBe(chat);
        expect(setupValue.ownership.has("session-new")).toBe(true);
        expect(
            setupValue.manager.findChatBySession("session-new"),
        ).toBe(chat);

        createdGate.resolve();
        await expect(creating).rejects.toBeInstanceOf(SessionCreationError);
        await waitFor(() => !setupValue.ownership.has("session-new"));
        await setupValue.manager.shutdown();
        expect(setupValue.manager.getChat(chat.id)).toBeUndefined();
        expect(
            setupValue.manager.findChatBySession("session-new"),
        ).toBeUndefined();
    });

    test("shutdown does not close shared hosts while a chat close is incomplete", async () => {
        const setupValue = setup(
            (port) => {
                port.closeError = new Error("activation close unconfirmed");
            },
            { activationCloseTimeoutMs: 50 },
            { kind: "wasm", moduleUrl: "about:blank" },
        );
        setupValue.database.seed(directoryEntry("session-1"));
        const chat = setupValue.manager.createChat();
        await chat.execute(ChatUserAction.changeSessionTo("session-1"));

        let closeAllCalls = 0;
        (
            setupValue.manager as unknown as {
                wasmRuntimes: { closeAll(): Promise<[]> };
            }
        ).wasmRuntimes = {
            closeAll: async () => {
                closeAllCalls += 1;
                return [];
            },
        };

        await expect(setupValue.manager.shutdown()).rejects.toBeInstanceOf(
            AggregateError,
        );
        expect(closeAllCalls).toBe(0);
        expect(setupValue.ownership.has("session-1")).toBe(true);
        expect(setupValue.manager.getChat(chat.id)).toBe(chat);

        delete setupValue.ports[0].closeError;
        await setupValue.manager.shutdown();
        expect(closeAllCalls).toBe(1);
        expect(setupValue.manager.getChat(chat.id)).toBeUndefined();
        expect(setupValue.ownership.size).toBe(0);
    });
});
