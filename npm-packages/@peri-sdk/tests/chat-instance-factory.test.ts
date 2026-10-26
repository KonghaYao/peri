import { describe, expect, test } from "bun:test";
import {
    PeriHostCleanupError,
    PeriInstance,
    type PeriInstanceOptions,
} from "../src/agent/peri-instance";
import { ChatUserAction } from "../src/chat/action";
import { AtomicSessionClaims } from "../src/chat/claims";
import { createChatInstance } from "../src/chat/instance-factory";
import { ChatManager } from "../src/chat/manager";
import type { ChatInstanceContext } from "../src/chat/service-support";
import { MemoryKV } from "../src/kv/memory-kv";
import type { JsonRpcNotification, Transport } from "../src/transport/types";
import { FakeDatabase, directoryEntry } from "./chat-fixtures";

class TestTransport implements Transport {
    readonly requests: Array<{ method: string; params?: unknown }> = [];
    closeCalls = 0;
    closeError?: unknown;
    private readonly listeners = new Set<(event: JsonRpcNotification) => void>();

    async request<T>(method: string, params?: unknown): Promise<T> {
        this.requests.push({ method, params });
        if (method === "initialize") return { protocolVersion: 1 } as T;
        if (method === "session/new") return { sessionId: "session-new" } as T;
        if (method === "session/load") return { sessionId: (params as { sessionId: string }).sessionId } as T;
        if (method === "session/input/snapshot") return { generation: "g1" } as T;
        if (method === "session/close") return {} as T;
        throw new Error(`Unexpected request: ${method}`);
    }

    async sendRequest<T>(method: string, params?: unknown): Promise<{ response: Promise<T> }> {
        return { response: this.request<T>(method, params) };
    }

    async notify(): Promise<void> {}

    subscribe(listener: (event: JsonRpcNotification) => void): () => void {
        this.listeners.add(listener);
        return () => this.listeners.delete(listener);
    }

    emit(notification: JsonRpcNotification): void {
        for (const listener of this.listeners) listener(notification);
    }

    async *events(): AsyncIterable<JsonRpcNotification> {}

    setRequestHandler(): void {}

    async close(): Promise<void> {
        this.closeCalls += 1;
        if (this.closeError) throw this.closeError;
    }
}

function context(
    overrides: Partial<ChatInstanceContext> = {},
): ChatInstanceContext {
    return {
        chatId: "chat-1",
        path: "/handle-path",
        agentConfig: {},
        defaults: {},
        host: {
            kind: "transport",
            factory: () => {
                throw new Error("host factory must be provided");
            },
        },
        signal: new AbortController().signal,
        database: new FakeDatabase(),
        lifecycle: {
            closeDrainTimeoutMs: 3000,
            startupSettleTimeoutMs: 5000,
            activationCloseTimeoutMs: 5000,
            shutdownChatTimeoutMs: 15000,
        },
        acquireWasmHost: () => {
            throw new Error("WASM host must not be acquired in transport tests");
        },
        ...overrides,
    };
}

describe("createChatInstance", () => {
    test("transport host receives the handle path and close confirms transport shutdown", async () => {
        const transport = new TestTransport();
        const calls: Array<{ path: string; aborted: boolean }> = [];
        const instance = createChatInstance(context({
            host: {
                kind: "transport",
                factory: (path, options) => {
                    calls.push({ path, aborted: options?.signal?.aborted ?? false });
                    return transport;
                },
            },
        }));

        await instance.bindNew({ path: "/workspace" });
        expect(calls).toEqual([{ path: "/workspace", aborted: false }]);
        await instance.close();

        expect(transport.closeCalls).toBe(1);
        expect(instance.running).toBe(false);
    });

    test("a failed transport close keeps the resource and surfaces the cause", async () => {
        const transport = new TestTransport();
        const failure = new Error("transport close failed");
        transport.closeError = failure;
        const instance = createChatInstance(context({
            host: { kind: "transport", factory: () => transport },
        }));

        await instance.bindNew({ path: "/workspace" });
        await expect(instance.close()).rejects.toBeInstanceOf(PeriHostCleanupError);
        expect(instance.needsReload).toBe(true);

        transport.closeError = undefined;
        await instance.close();
        expect(transport.closeCalls).toBe(2);
    });

    test("WASM host uses the lease transport and maps its close report", async () => {
        const transport = new TestTransport();
        let closed = 0;
        let signal: AbortSignal | undefined;
        const instance = createChatInstance(context({
            host: { kind: "wasm" },
            acquireWasmHost: async (value) => {
                signal = value;
                return {
                    transport,
                    close: async () => {
                        closed += 1;
                        return { confirmed: true, generationId: "generation-1" };
                    },
                } as never;
            },
        }));

        await instance.bindNew({ path: "/workspace" });
        expect(signal?.aborted).toBe(false);
        await instance.close();

        expect(closed).toBe(1);
        expect(transport.closeCalls).toBe(0);
    });

    test("loading a session reads the persisted cwd and never falls back to the handle path", async () => {
        const transport = new TestTransport();
        const database = new FakeDatabase();
        database.seed(directoryEntry("session-1", "/persisted"));
        const paths: string[] = [];
        const instance = createChatInstance(context({
            database,
            host: {
                kind: "transport",
                factory: (path) => {
                    paths.push(path);
                    return transport;
                },
            },
        }));

        await instance.bindExisting("session-1");
        expect(database.getCalls).toEqual(["session-1"]);
        expect(paths).toEqual(["/persisted"]);
        await instance.close();
    });

    test("an aborted startup signal rejects before acquiring a host", async () => {
        const controller = new AbortController();
        controller.abort(new Error("cancelled"));
        let acquired = 0;
        const instance = createChatInstance(context({
            signal: controller.signal,
            host: {
                kind: "transport",
                factory: () => {
                    acquired += 1;
                    return new TestTransport();
                },
            },
        }));

        await expect(instance.bindNew({ path: "/workspace" })).rejects.toMatchObject({
            name: "PeriInstanceBindError",
        });
        expect(acquired).toBe(0);
    });
});

describe("default factory wiring", () => {
    test("ChatManager assembles the default PeriInstance when no factory is injected", async () => {
        const transport = new TestTransport();
        const paths: string[] = [];
        const manager = new ChatManager({
            agentConfig: {},
            database: new FakeDatabase(),
            sessionClaims: new AtomicSessionClaims(new MemoryKV()),
            host: {
                kind: "transport",
                factory: (path) => {
                    paths.push(path);
                    return transport;
                },
            },
            defaults: { path: "/workspace" },
        });
        const chat = manager.createChat();

        const ready = await chat.execute(ChatUserAction.createNewSession({}));
        expect(ready.sessionId).toBe("session-new");
        expect(paths).toEqual(["/workspace"]);
        expect(transport.requests.map((request) => request.method)).toContain("session/new");

        await chat.execute(ChatUserAction.cancel({}));
        await manager.shutdown();
        expect(transport.closeCalls).toBe(1);
        expect(manager.getChat(chat.id)).toBeUndefined();
    });

    test("ChatService reads real PeriInstance payloads and reports integrity mismatches", async () => {
        const transport = new TestTransport();
        const manager = new ChatManager({
            agentConfig: {},
            database: new FakeDatabase(),
            sessionClaims: new AtomicSessionClaims(new MemoryKV()),
            host: { kind: "transport", factory: () => transport },
            defaults: { path: "/workspace" },
        });
        const chat = manager.createChat();
        const notices: string[] = [];
        chat.subscribeFacts({
            onBinding(_target, replay) {
                for (const envelope of replay) {
                    if (envelope.fact.kind === "notice") notices.push(JSON.stringify(envelope.fact.notification));
                }
            },
            onFact(envelope) {
                if (envelope.fact.kind === "notice") notices.push(JSON.stringify(envelope.fact.notification));
            },
            onClosed() {},
        });
        await chat.execute(ChatUserAction.createNewSession({}));
        const argumentsValue = { command: "x".repeat(5000) };
        transport.emit({
            jsonrpc: "2.0",
            method: "session/update",
            params: {
                sessionId: "session-new",
                update: {
                    sessionUpdate: "tool_call",
                    toolCallId: "tool-1",
                    title: "Shell",
                    rawInput: argumentsValue,
                    status: "pending",
                },
            },
        });
        const update = JSON.parse(notices.at(-1)!) as {
            params: { update: { argumentsRef?: { id: string; bytes: number } } };
        };
        const ref = update.params.update.argumentsRef!;
        expect(ref).toBeDefined();
        await expect(chat.readPayload(ref)).resolves.toMatchObject({
            id: ref.id,
            bytes: ref.bytes,
            json: JSON.stringify(argumentsValue),
        });
        await expect(chat.readPayload({ id: ref.id, bytes: ref.bytes + 1 }))
            .rejects.toMatchObject({ name: "PayloadReadError", code: "integrity_error" });
        await manager.shutdown();
    });
});

test("PeriInstance options type keeps the factory output assignable", () => {
    const instance: PeriInstance = createChatInstance(context());
    const options: PeriInstanceOptions | undefined = undefined;
    expect(instance).toBeInstanceOf(PeriInstance);
    expect(options).toBeUndefined();
});
