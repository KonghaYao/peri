import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import {
    ChatActionValidationError,
    ChatClosedError,
} from "../src/chat/errors";
import type { ChatFactEnvelope, ChatFactObserver } from "../src/contracts/chat";
import { ChatManager } from "../src/chat/manager";
import type { ChatManagerInternalOptions } from "../src/chat/manager";
import type { SendReceipt } from "../src/chat/types";
import { PayloadReadError } from "../src/payload/reader";
import {
    FakeDatabase,
    FakePort,
    directoryEntry,
} from "./chat-fixtures";
import { OwnershipProbe } from "./ownership-probe";

/** 测试用 workspace 根；路径判断是纯字符串，不依赖真实目录。 */
const TEST_WORKSPACE = { id: "ws-test", runtimePath: "/" };

function setup(
    configure?: (port: FakePort, index: number) => void,
): {
    manager: ChatManager;
    ownership: OwnershipProbe;
    database: FakeDatabase;
    ports: FakePort[];
} {
    const ownership = new OwnershipProbe();
    const database = new FakeDatabase();
    const ports: FakePort[] = [];
    const options: ChatManagerInternalOptions = {
        store: { storage: {}, machineId: "test-machine" },
        database,
        kv: ownership,
        chatConfig: {
            runtime: {
                kind: "transport",
                factory: () => {
                    throw new Error("ACP transport must not start");
                },
            },
            session: {},
        },
        instanceFactory: () => {
            const port = new FakePort();
            configure?.(port, ports.length);
            ports.push(port);
            return port;
        },
    };
    const manager = new ChatManager(options);
    return { manager, ownership, database, ports };
}

async function readyChat(
    configure?: (port: FakePort, index: number) => void,
): Promise<{
    chat: ReturnType<ChatManager["createChat"]>;
    port: FakePort;
    setupValue: ReturnType<typeof setup>;
}> {
    const setupValue = setup(configure);
    const chat = setupValue.manager.createChat({ workspace: TEST_WORKSPACE, agent: {} });
    await chat.execute(ChatUserAction.createNewSession());
    return { chat, port: setupValue.ports[0], setupValue };
}

function recorder(): {
    observer: ChatFactObserver;
    bindings: Array<{ target: unknown; replay: readonly ChatFactEnvelope[] }>;
    facts: ChatFactEnvelope[];
    closed: () => boolean;
} {
    const bindings: Array<{
        target: unknown;
        replay: readonly ChatFactEnvelope[];
    }> = [];
    const facts: ChatFactEnvelope[] = [];
    let closed = false;
    return {
        observer: {
            onBinding(target, replay) {
                bindings.push({ target, replay });
            },
            onFact(envelope) {
                facts.push(envelope);
            },
            onClosed() {
                closed = true;
            },
        },
        bindings,
        facts,
        closed: () => closed,
    };
}

describe("send receipt correlation", () => {
    test("execute returns synchronously; accepted is not delivery", async () => {
        let releaseSend!: () => void;
        const sendGate = new Promise<void>((resolve) => {
            releaseSend = resolve;
        });
        const { chat, port } = await readyChat((candidate) => {
            candidate.sendGate = sendGate;
        });

        const receipt = chat.execute(
            ChatUserAction.send({ text: "hello", requestId: "send-1" }),
        ) as SendReceipt;

        expect(receipt.requestId).toBe("send-1");
        expect(receipt.inputId).toBeTruthy();
        expect(receipt.target).toEqual({
            chatId: chat.id,
            sessionId: "session-new",
            bindingRevision: 1,
        });
        expect(receipt.isSent).toBe(false);

        releaseSend();
        await receipt.accepted;
        expect(receipt.isSent).toBe(false);
        expect(port.sendCalls).toEqual([
            { inputId: receipt.inputId, text: "hello" },
        ]);

        port.deliver(receipt.inputId, "hello");
        await receipt;
        expect(receipt.isSent).toBe(true);
    });

    test("take_back rejects the matching delivery waiter but keeps accepted", async () => {
        const { chat, port } = await readyChat();
        const receipt = chat.execute(
            ChatUserAction.send({ text: "hello" }),
        ) as SendReceipt;
        await receipt.accepted;

        const result = await chat.execute(
            ChatUserAction.takeBack(receipt.inputId),
        );

        expect(result.status).toBe("accepted");
        expect(port.takeBacks).toEqual([receipt.inputId]);
        await expect(Promise.resolve(receipt)).rejects.toMatchObject({
            code: "input_withdrawn",
        });
    });

    test("close rejects a pending delivery with ChatClosedError", async () => {
        let releaseSend!: () => void;
        const sendGate = new Promise<void>((resolve) => {
            releaseSend = resolve;
        });
        const { chat } = await readyChat((candidate) => {
            candidate.sendGate = sendGate;
        });
        const receipt = chat.execute(
            ChatUserAction.send({ text: "pending" }),
        ) as SendReceipt;

        await chat.close();
        await expect(Promise.resolve(receipt)).rejects.toBeInstanceOf(
            ChatClosedError,
        );
        releaseSend();
    });

    test("late results keep the binding target they were admitted for", async () => {
        const { chat, setupValue } = await readyChat();
        setupValue.database.seed(directoryEntry("session-b"));

        const first = chat.execute(
            ChatUserAction.send({ text: "a" }),
        ) as SendReceipt;
        await first.accepted;
        setupValue.ports[0].deliver(first.inputId, "a");
        await first;
        await chat.execute(ChatUserAction.changeSessionTo("session-b"));

        const second = chat.execute(
            ChatUserAction.send({ text: "b" }),
        ) as SendReceipt;
        await second.accepted;
        setupValue.ports[1].deliver(second.inputId, "b");
        await second;

        expect(first.target.sessionId).toBe("session-new");
        expect(first.target.bindingRevision).toBe(1);
        expect(second.target.sessionId).toBe("session-b");
        expect(second.target.bindingRevision).toBe(2);
    });
});

describe("control actions", () => {
    test("cancel reports requested or idle without inventing stop semantics", async () => {
        const { chat, port } = await readyChat();
        expect((await chat.execute(ChatUserAction.cancel())).status).toBe(
            "idle",
        );

        port.runningValue = true;
        port.cancelStatus = "requested";
        expect((await chat.execute(ChatUserAction.cancel())).status).toBe(
            "requested",
        );
        expect(port.calls.filter((call) => call === "cancel")).toHaveLength(2);
    });

    test("waiting inputs do not serialize or cross-wire concurrent cancel and answer", async () => {
        let releaseA!: () => void;
        let releaseB!: () => void;
        const gateA = new Promise<void>((resolve) => {
            releaseA = resolve;
        });
        const gateB = new Promise<void>((resolve) => {
            releaseB = resolve;
        });
        const setupValue = setup((port, index) => {
            port.bindNewSessionId = index === 0 ? "session-a" : "session-b";
            port.sendGate = index === 0 ? gateA : gateB;
            port.runningValue = true;
            port.pendingInteractionValue = 1;
        });
        const chatA = setupValue.manager.createChat({ workspace: TEST_WORKSPACE, agent: {} });
        const chatB = setupValue.manager.createChat({ workspace: TEST_WORKSPACE, agent: {} });
        await chatA.execute(ChatUserAction.createNewSession());
        await chatB.execute(ChatUserAction.createNewSession());        const [portA, portB] = setupValue.ports;

        const inputA = chatA.execute(
            ChatUserAction.send({ text: "a", requestId: "send-a" }),
        ) as SendReceipt;
        const inputB = chatB.execute(
            ChatUserAction.send({ text: "b", requestId: "send-b" }),
        ) as SendReceipt;
        while (portA.sendCalls.length === 0 || portB.sendCalls.length === 0) {
            await new Promise<void>((resolve) => setTimeout(resolve, 0));
        }

        const [cancel, answer] = await Promise.all([
            chatA.execute(
                ChatUserAction.cancel({ requestId: "cancel-a" }),
            ),
            chatB.execute(
                ChatUserAction.answer({
                    interactionId: "interaction-b",
                    answer: { kind: "permission", decision: "allow_once" },
                    requestId: "answer-b",
                }),
            ),
        ]);

        expect(cancel.status).toBe("requested");
        expect(answer.status).toBe("accepted");
        expect(portA.calls.filter((call) => call === "cancel")).toHaveLength(1);
        expect(portA.calls.some((call) => call.startsWith("answer:"))).toBe(
            false,
        );
        expect(portA.answers).toEqual([]);
        expect(portB.calls.filter((call) => call === "cancel")).toHaveLength(0);
        expect(portB.answers).toEqual([
            {
                interactionId: "interaction-b",
                answer: { kind: "permission", decision: "allow_once" },
            },
        ]);
        // cancel 只是请求，不取消另一会话，也不撤回等待中的输入。
        expect(portA.running).toBe(true);
        expect(portB.running).toBe(true);
        expect(inputA.isSent).toBe(false);
        expect(inputB.isSent).toBe(false);

        releaseA();
        releaseB();
        await Promise.all([inputA.accepted, inputB.accepted]);
        portA.deliver(inputA.inputId, "a");
        portB.deliver(inputB.inputId, "b");
        await Promise.all([inputA, inputB]);
        expect(inputA.isSent).toBe(true);
        expect(inputB.isSent).toBe(true);
    });

    test("force_send, answer and history forward to the active binding", async () => {
        const { chat, port } = await readyChat();
        port.historyResult = { applied: 2, nextCursor: "cursor-2" };

        expect(
            (await chat.execute(ChatUserAction.forceSend("input-1"))).status,
        ).toBe("accepted");
        expect(port.dispatches).toEqual(["input-1"]);

        expect(
            (
                await chat.execute(
                    ChatUserAction.answer({
                        interactionId: "interaction-1",
                        answer: {
                            kind: "permission",
                            decision: "allow_once",
                        },
                    }),
                )
            ).status,
        ).toBe("accepted");
        expect(port.answers[0].interactionId).toBe("interaction-1");

        expect(
            (
                await chat.execute(
                    ChatUserAction.setConfigOption({
                        configId: "thinking_effort",
                        value: "high",
                    }),
                )
            ).status,
        ).toBe("accepted");
        expect(port.configOptions).toEqual([
            { configId: "thinking_effort", value: "high" },
        ]);

        await expect(
            chat.execute(ChatUserAction.loadEarlierHistory({ limit: 20 })),
        ).resolves.toEqual({ applied: 2, nextCursor: "cursor-2" });
    });

    test("port failures are wrapped with requestId and a disposition", async () => {
        const { chat, port } = await readyChat();
        port.historyError = new Error("history transport failed");

        await expect(
            chat.execute(
                ChatUserAction.loadEarlierHistory({ requestId: "hist-1" }),
            ),
        ).rejects.toMatchObject({
            requestId: "hist-1",
            code: "history_failed",
            disposition: "unknown",
        });
    });

    test("set_config_option failures keep the request identity", async () => {
        const { chat, port } = await readyChat();
        port.configOptionError = new Error("config rejected");

        await expect(
            chat.execute(
                ChatUserAction.setConfigOption({
                    configId: "model", value: "fast", requestId: "config-1",
                }),
            ),
        ).rejects.toMatchObject({
            requestId: "config-1",
            code: "set_config_option_failed",
            disposition: "unknown",
        });
    });
});

describe("action validation and closed handles", () => {
    test("plain wire objects are parsed; invalid shapes fail with invalid_action", async () => {
        const { chat } = await readyChat();

        expect(() =>
            chat.execute({
                type: "send",
                requestId: "wire-1",
                payload: { text: "" },
            } as never),
        ).toThrow(ChatActionValidationError);

        const receipt = chat.execute({
            type: "send",
            requestId: "wire-2",
            payload: { text: "ok" },
        } as never) as SendReceipt;
        expect(receipt.requestId).toBe("wire-2");
    });

    test("closed handles reject new actions explicitly", async () => {
        const { chat } = await readyChat();
        await chat.close();
        expect(() =>
            chat.execute(ChatUserAction.send({ text: "after close" })),
        ).toThrow(ChatClosedError);
    });
});

describe("facts and late views", () => {
    test("subscribe replays initial facts, then live facts per binding revision", async () => {
        const early = recorder();
        let chat!: ReturnType<ChatManager["createChat"]>;
        const setupValue = setup((port, index) => {
            port.bindNewFacts = [
                { kind: "run-state", running: false, executionBlocked: false },
            ];
            if (index === 1) {
                port.bindExistingFacts = [
                    { kind: "notice", notification: {
                        jsonrpc: "2.0",
                        method: "session/update",
                    } },
                ];
            }
        });
        chat = setupValue.manager.createChat({ workspace: TEST_WORKSPACE, agent: {} });
        chat.subscribeFacts(early.observer);
        await chat.execute(ChatUserAction.createNewSession());

        expect(early.bindings).toHaveLength(1);
        expect(early.bindings[0].replay).toHaveLength(1);
        expect(early.bindings[0].replay[0].bindingRevision).toBe(1);
        expect(early.facts).toHaveLength(0);

        const late = recorder();
        chat.subscribeFacts(late.observer);
        expect(late.bindings).toHaveLength(1);
        expect(late.bindings[0].replay).toHaveLength(1);

        setupValue.ports[0].emit({
            kind: "interaction-settled",
            id: "i-1",
            status: "resolved",
        });
        expect(early.facts).toHaveLength(1);
        expect(early.facts[0].sequence).toBe(2);
        expect(late.facts).toHaveLength(1);

        setupValue.database.seed(directoryEntry("session-b"));
        await chat.execute(ChatUserAction.changeSessionTo("session-b"));
        expect(early.bindings).toHaveLength(2);
        expect(early.bindings[1].replay[0].bindingRevision).toBe(2);
        expect(early.bindings[1].replay[0].sequence).toBe(1);

        await chat.close();
        expect(early.closed()).toBe(true);
        expect(late.closed()).toBe(true);
    });
});

describe("payload reads", () => {
    test("delegates to the running payload store and validates the response", async () => {
        const { chat, port } = await readyChat();
        const json = '{"answer":42}';
        const bytes = new TextEncoder().encode(json).byteLength;
        port.payloadReader = async (ref) => ({ ...ref, json });

        await expect(
            chat.readPayload({ id: "payload-1", bytes }),
        ).resolves.toEqual({ id: "payload-1", bytes, json });

        await expect(
            chat.readPayload({ id: "payload-1", bytes }, { maxBytes: 1 }),
        ).rejects.toBeInstanceOf(PayloadReadError);

        port.payloadReader = async (ref) => ({
            id: "other",
            bytes: ref.bytes,
            json,
        });
        await expect(
            chat.readPayload({ id: "payload-1", bytes }),
        ).rejects.toMatchObject({ code: "integrity_error" });
    });

    test("closed handles report unavailable without probing capabilities", async () => {
        const { chat } = await readyChat();
        await chat.close();
        await expect(
            chat.readPayload({ id: "payload-1", bytes: 1 }),
        ).rejects.toMatchObject({ code: "unavailable" });
    });
});
