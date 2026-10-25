import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import { AtomicSessionClaims } from "../src/chat/claims";
import {
    ChatBusyError,
    ChatNotReadyError,
} from "../src/chat/errors";
import { ChatManager } from "../src/chat/manager";
import type { ChatManagerInternalOptions } from "../src/chat/manager";
import type { ChatLifecycleOptions, SendReceipt } from "../src/chat/types";
import { MemoryKV } from "../src/kv/memory-kv";
import { DeadlineError } from "../src/util/deadline";
import {
    FakeDatabase,
    FakePort,
    directoryEntry,
} from "./chat-fixtures";

interface Setup {
    readonly manager: ChatManager;
    readonly claims: AtomicSessionClaims;
    readonly database: FakeDatabase;
    readonly ports: FakePort[];
}

function setup(
    configure?: (port: FakePort) => void,
    claims?: AtomicSessionClaims,
    lifecycle?: ChatLifecycleOptions,
): Setup {
    const kv = new MemoryKV();
    const database = new FakeDatabase();
    const portList: FakePort[] = [];
    const managerClaims = claims ?? new AtomicSessionClaims(kv);
    const options: ChatManagerInternalOptions = {
        agentConfig: {},
        database,
        sessionClaims: managerClaims,
        host: {
            kind: "transport",
            factory: () => {
                throw new Error("ACP transport must not start");
            },
        },
        defaults: { path: "/workspace" },
        ...(lifecycle ? { lifecycle } : {}),
        instanceFactory: () => {
            const port = new FakePort();
            configure?.(port);
            portList.push(port);
            return port;
        },
    };
    return {
        manager: new ChatManager(options),
        claims: managerClaims,
        database,
        ports: portList,
    };
}

async function toggle(
    setupValue: Setup,
    sessionId: string,
    requestId = `load-${sessionId}`,
) {
    const chat = setupValue.manager.createChat();
    await chat.execute(
        ChatUserAction.changeSessionTo(sessionId, { requestId }),
    );
    return chat;
}

async function waitFor(predicate: () => boolean): Promise<void> {
    for (let attempt = 0; attempt < 100; attempt += 1) {
        if (predicate()) return;
        await Promise.resolve();
    }
    throw new Error("condition was not reached");
}

describe("binding idle gate", () => {
    test("running chat rejects switch without cancelling or closing A", async () => {
        const setupValue = setup();
        setupValue.database.seed(directoryEntry("session-a"));
        setupValue.database.seed(directoryEntry("session-b"));
        const chat = await toggle(setupValue, "session-a");
        const portA = setupValue.ports[0];
        portA.runningValue = true;

        await expect(
            chat.execute(ChatUserAction.changeSessionTo("session-b")),
        ).rejects.toBeInstanceOf(ChatBusyError);

        expect(chat.sessionId).toBe("session-a");
        expect(chat.bindingRevision).toBe(1);
        expect(portA.calls).not.toContain("cancel");
        expect(portA.closeCalls).toBe(0);
        expect(setupValue.ports).toHaveLength(1);
        expect(setupValue.database.getCalls).toContain("session-a");
    });

    test("pending input count and pending interactions block switching", async () => {
        const setupValue = setup();
        setupValue.database.seed(directoryEntry("session-a"));
        setupValue.database.seed(directoryEntry("session-b"));
        const chat = await toggle(setupValue, "session-a");
        const port = setupValue.ports[0];

        port.pendingInputValue = 1;
        await expect(
            chat.execute(ChatUserAction.changeSessionTo("session-b")),
        ).rejects.toBeInstanceOf(ChatBusyError);

        port.pendingInputValue = 0;
        port.pendingInteractionValue = 1;
        await expect(
            chat.execute(ChatUserAction.changeSessionTo("session-b")),
        ).rejects.toBeInstanceOf(ChatBusyError);
        expect(chat.sessionId).toBe("session-a");
    });

    test("a synchronously admitted send blocks switching until it settles", async () => {
        let releaseSend!: () => void;
        const sendGate = new Promise<void>((resolve) => {
            releaseSend = resolve;
        });
        const setupValue = setup((port) => {
            port.sendGate = sendGate;
        });
        setupValue.database.seed(directoryEntry("session-a"));
        setupValue.database.seed(directoryEntry("session-b"));
        const chat = await toggle(setupValue, "session-a");
        const port = setupValue.ports[0];

        const receipt = chat.execute(
            ChatUserAction.send({ text: "hello", requestId: "send-1" }),
        ) as SendReceipt;
        await expect(
            chat.execute(ChatUserAction.changeSessionTo("session-b")),
        ).rejects.toBeInstanceOf(ChatBusyError);

        releaseSend();
        await receipt.accepted;
        port.deliver(receipt.inputId, "hello");
        await receipt;

        const switched = await chat.execute(
            ChatUserAction.changeSessionTo("session-b"),
        );
        expect(switched.sessionId).toBe("session-b");
        expect(chat.bindingRevision).toBe(2);
    });

    test("needsReload blocks switching", async () => {
        const setupValue = setup();
        setupValue.database.seed(directoryEntry("session-a"));
        setupValue.database.seed(directoryEntry("session-b"));
        const chat = await toggle(setupValue, "session-a");
        setupValue.ports[0].needsReloadValue = true;

        await expect(
            chat.execute(ChatUserAction.changeSessionTo("session-b")),
        ).rejects.toBeInstanceOf(ChatNotReadyError);
        expect(chat.sessionId).toBe("session-a");
    });
});

describe("binding concurrency and revision", () => {
    test("same ready target is a no-op and does not increment revision", async () => {
        const setupValue = setup();
        setupValue.database.seed(directoryEntry("session-a"));
        const chat = await toggle(setupValue, "session-a");
        const port = setupValue.ports[0];
        const bindCalls = port.calls.filter((call) =>
            call.startsWith("bindExisting"),
        ).length;

        const result = await chat.execute(
            ChatUserAction.changeSessionTo("session-a", {
                requestId: "again",
            }),
        );

        expect(result.sessionId).toBe("session-a");
        expect(result.requestId).toBe("again");
        expect(chat.bindingRevision).toBe(1);
        expect(
            port.calls.filter((call) => call.startsWith("bindExisting")),
        ).toHaveLength(bindCalls);
    });

    test("concurrent loads of the same target merge into one ACP load", async () => {
        let release!: () => void;
        const gate = new Promise<void>((resolve) => {
            release = resolve;
        });
        const setupValue = setup((port) => {
            port.bindExistingGate = gate;
        });
        setupValue.database.seed(directoryEntry("session-a"));
        const chat = setupValue.manager.createChat();

        const first = chat.execute(
            ChatUserAction.changeSessionTo("session-a", {
                requestId: "first",
            }),
        );
        const second = chat.execute(
            ChatUserAction.changeSessionTo("session-a", {
                requestId: "second",
            }),
        );

        await waitFor(() => setupValue.ports.length === 1);
        await setupValue.ports[0].bindExistingStarted;
        release();
        const [firstResult, secondResult] = await Promise.all([first, second]);

        expect(firstResult.requestId).toBe("first");
        expect(secondResult.requestId).toBe("second");
        expect(firstResult.bindingRevision).toBe(1);
        expect(secondResult.bindingRevision).toBe(1);
        expect(
            setupValue.ports[0].calls.filter((call) =>
                call.startsWith("bindExisting"),
            ),
        ).toHaveLength(1);
    });

    test("index is visible during path resolution and blocks another handle", async () => {
        const setupValue = setup();
        setupValue.database.seed(directoryEntry("session-a"));
        const originalGetSession = setupValue.database.getSession.bind(
            setupValue.database,
        );
        let releasePath!: () => void;
        const pathGate = new Promise<void>((resolve) => {
            releasePath = resolve;
        });
        let pathStarted!: () => void;
        const started = new Promise<void>((resolve) => {
            pathStarted = resolve;
        });
        setupValue.database.getSession = async (id) => {
            pathStarted();
            await pathGate;
            return originalGetSession(id);
        };

        const chat = setupValue.manager.createChat();
        const loading = chat.execute(
            ChatUserAction.changeSessionTo("session-a", {
                requestId: "loading",
            }),
        );
        await started;

        expect(chat.phase).toBe("switching");
        expect(setupValue.manager.findChatBySession("session-a")).toBe(chat);
        // 索引先于 claim：path gate 期间 claim 尚未发生。
        expect(setupValue.claims.has("session-a")).toBe(false);

        const contender = setupValue.manager.createChat();
        await expect(
            contender.execute(
                ChatUserAction.changeSessionTo("session-a", {
                    requestId: "contender",
                }),
            ),
        ).rejects.toMatchObject({
            code: "binding_conflict",
            ownerChatId: chat.id,
        });

        releasePath();
        await loading;
        expect(setupValue.manager.findChatBySession("session-a")).toBe(chat);
    });

    test("successful switch closes A, releases its claim and increments revision", async () => {
        const setupValue = setup();
        setupValue.database.seed(directoryEntry("session-a"));
        setupValue.database.seed(directoryEntry("session-b"));
        const chat = await toggle(setupValue, "session-a");
        const portA = setupValue.ports[0];

        const result = await chat.execute(
            ChatUserAction.changeSessionTo("session-b"),
        );

        expect(result.bindingRevision).toBe(2);
        expect(chat.sessionId).toBe("session-b");
        expect(portA.closeCalls).toBe(1);
        expect(setupValue.claims.has("session-a")).toBe(false);
        expect(setupValue.claims.has("session-b")).toBe(true);
        expect(setupValue.ports).toHaveLength(2);
        expect(setupValue.ports[1].calls).toContain(
            "bindExisting:session-b",
        );
    });

    test("candidate failure keeps the old binding, revision and claim", async () => {
        let attempts = 0;
        const setupValue = setup((port) => {
            attempts += 1;
            if (attempts === 2) {
                port.bindExistingError = new Error("candidate failed");
            }
        });
        setupValue.database.seed(directoryEntry("session-a"));
        setupValue.database.seed(directoryEntry("session-b"));
        const chat = await toggle(setupValue, "session-a");
        const portA = setupValue.ports[0];

        await expect(
            chat.execute(ChatUserAction.changeSessionTo("session-b")),
        ).rejects.toThrow("candidate failed");

        expect(chat.sessionId).toBe("session-a");
        expect(chat.bindingRevision).toBe(1);
        expect(chat.phase).toBe("ready");
        expect(setupValue.claims.has("session-a")).toBe(true);
        expect(setupValue.claims.has("session-b")).toBe(false);
        expect(portA.closeCalls).toBe(0);
        expect(setupValue.ports).toHaveLength(2);
        expect(setupValue.ports[1].closeCalls).toBe(1);
    });

    test("claim release failure is preserved in the binding error cause", async () => {
        const kv = new MemoryKV();
        const claims = new AtomicSessionClaims(kv);
        const originalRelease = claims.release.bind(claims);
        claims.release = async (sessionId) => {
            if (sessionId === "session-1")
                throw new Error("claim release failed");
            return originalRelease(sessionId);
        };
        const setupValue = setup(
            (port) => {
                port.bindExistingError = new Error("load failed");
            },
            claims,
        );
        setupValue.database.seed(directoryEntry("session-1"));
        const chat = setupValue.manager.createChat();

        let failure: unknown;
        try {
            await chat.execute(
                ChatUserAction.changeSessionTo("session-1", {
                    requestId: "load-1",
                }),
            );
        } catch (error) {
            failure = error;
        }

        expect(failure).toMatchObject({
            code: "binding_failed",
            disposition: "unknown",
        });
        const cause = (failure as { cause?: unknown }).cause;
        expect(cause).toBeInstanceOf(AggregateError);
        expect(
            ((cause as AggregateError).errors as Error[]).map(
                (error) => error.message,
            ),
        ).toEqual(["load failed", "claim release failed"]);
        expect(chat.phase).toBe("cleanup-pending");
        expect(setupValue.manager.findChatBySession("session-1")).toBe(chat);
        expect(claims.has("session-1")).toBe(true);
    });

    test("old close failure blocks the handle and keeps the old claim", async () => {
        const setupValue = setup();
        setupValue.database.seed(directoryEntry("session-a"));
        setupValue.database.seed(directoryEntry("session-b"));
        const chat = await toggle(setupValue, "session-a");
        const portA = setupValue.ports[0];
        portA.closeError = new Error("old close unconfirmed");

        await expect(
            chat.execute(ChatUserAction.changeSessionTo("session-b")),
        ).rejects.toThrow("old close unconfirmed");

        expect(chat.phase).toBe("blocked");
        expect(chat.sessionId).toBe("session-a");
        expect(chat.bindingRevision).toBe(1);
        expect(setupValue.claims.has("session-a")).toBe(true);
        expect(setupValue.claims.has("session-b")).toBe(false);
        expect(setupValue.ports[1].closeCalls).toBe(1);
        expect(() =>
            chat.execute(ChatUserAction.send({ text: "blocked" })),
        ).toThrow(ChatNotReadyError);

        delete portA.closeError;
        await chat.close();
        expect(setupValue.claims.size).toBe(0);
        expect(portA.closeCalls).toBe(2);
    });

    test("old close that never resolves is bounded and retryable after it settles", async () => {
        const setupValue = setup(undefined, undefined, {
            activationCloseTimeoutMs: 25,
        });
        setupValue.database.seed(directoryEntry("session-a"));
        setupValue.database.seed(directoryEntry("session-b"));
        const chat = await toggle(setupValue, "session-a");
        const portA = setupValue.ports[0];
        const originalClose = portA.close.bind(portA);
        let closeAttempts = 0;
        portA.close = () => {
            closeAttempts += 1;
            return new Promise<void>(() => {});
        };

        const started = performance.now();
        let failure: unknown;
        try {
            await chat.execute(ChatUserAction.changeSessionTo("session-b"));
        } catch (error) {
            failure = error;
        }
        const elapsed = performance.now() - started;

        expect(failure).toMatchObject({
            code: "binding_cleanup_failed",
            disposition: "unknown",
        });
        expect((failure as { cause?: unknown }).cause).toBeInstanceOf(
            DeadlineError,
        );
        expect(elapsed).toBeLessThan(1000);
        expect(chat.phase).toBe("blocked");
        expect(chat.sessionId).toBe("session-a");
        expect(chat.bindingRevision).toBe(1);
        expect(setupValue.claims.has("session-a")).toBe(true);
        expect(setupValue.manager.findChatBySession("session-a")).toBe(chat);
        expect(setupValue.manager.getChat(chat.id)).toBe(chat);
        // 新候选的清理已确认，因此只释放候选；旧激活的 claim/索引必须保留。
        expect(setupValue.ports[1].closeCalls).toBe(1);
        expect(setupValue.claims.has("session-b")).toBe(false);
        expect(closeAttempts).toBe(1);

        portA.close = originalClose;
        await chat.close();
        expect(setupValue.claims.size).toBe(0);
        expect(setupValue.manager.getChat(chat.id)).toBeUndefined();
        expect(
            setupValue.manager.findChatBySession("session-a"),
        ).toBeUndefined();
        expect(portA.closeCalls).toBe(1);
    });
});
