import { describe, expect, test } from "bun:test";
import {
    PeriHostCleanupError,
    PeriInstance,
    type PeriInstanceOptions,
} from "../src/agent/peri-instance";
import { ChatUserAction } from "../src/chat/action";
import { createChatInstance } from "../src/chat/instance-factory";
import { ChatManager } from "../src/chat/manager";
import type { ChatInstanceContext } from "../src/chat/service-context";
import type { ChatFact } from "../src/contracts/chat";
import type { WasmRuntimeAcquireOptions } from "../src/wasm/runtime-pool";
import { FakeDatabase, directoryEntry } from "./chat-fixtures";
import { TestTransport } from "./chat-transport-fixture";
import { OwnershipProbe } from "./ownership-probe";

function context(
    overrides: Partial<ChatInstanceContext> = {},
): ChatInstanceContext {
    return {
        chatId: "chat-1",
        path: "/handle-path",
        agentConfig: {},
        agentConfigJson: "{}",
        sessionInit: {},
        runtime: {
            kind: "transport",
            factory: () => {
                throw new Error("runtime factory must be provided");
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
        acquireWasmRuntime: () => {
            throw new Error("WASM runtime must not be acquired in transport tests");
        },
        ...overrides,
    };
}

describe("createChatInstance", () => {
    test("transport runtime receives the handle path and close confirms transport shutdown", async () => {
        const transport = new TestTransport();
        const calls: Array<{ path: string; aborted: boolean }> = [];
        const instance = createChatInstance(context({
            runtime: {
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
            runtime: { kind: "transport", factory: () => transport },
        }));

        await instance.bindNew({ path: "/workspace" });
        await expect(instance.close()).rejects.toBeInstanceOf(PeriHostCleanupError);
        expect(instance.needsReload).toBe(true);

        transport.closeError = undefined;
        await instance.close();
        expect(transport.closeCalls).toBe(2);
    });

    test("WASM runtime uses the per-chat startup config and maps its close report", async () => {
        const transport = new TestTransport();
        let closed = 0;
        let acquired: WasmRuntimeAcquireOptions | undefined;
        const diagnostic = (): void => {};
        const instance = createChatInstance(context({
            runtime: { kind: "wasm" },
            agentConfigJson: '{"cwd":"/runtime"}',
            runtimeEnv: { PERI_FLAG: "1" },
            onRuntimeDiagnostic: diagnostic,
            acquireWasmRuntime: async (value) => {
                acquired = value;
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
        expect(acquired?.signal?.aborted).toBe(false);
        expect(acquired?.configJson).toBe('{"cwd":"/runtime"}');
        expect(acquired?.env).toEqual({ PERI_FLAG: "1" });
        expect(acquired?.onDiagnostic).toBe(diagnostic);
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
            runtime: {
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

    test("payloadBudgets reach the PeriInstance payload store", async () => {
        const transport = new TestTransport();
        const facts: ChatFact[] = [];
        const instance = createChatInstance(context({
            runtime: { kind: "transport", factory: () => transport },
            payloadBudgets: {
                inlineBytes: 8,
                externalBytes: 1_000_000,
                maxPayloadBytes: 1_000_000,
            },
        }));
        instance.attach((fact) => facts.push(fact));
        await instance.bindNew({ path: "/workspace" });

        transport.emit({
            jsonrpc: "2.0",
            method: "session/update",
            params: {
                update: {
                    sessionUpdate: "tool_call",
                    toolCallId: "call-1",
                    title: "large input",
                    rawInput: { text: "x".repeat(200) },
                },
            },
        });

        const notice = facts.find(
            (fact): fact is Extract<ChatFact, { kind: "notice" }> =>
                fact.kind === "notice",
        );
        const update = (
            notice!.notification.params as {
                update: { rawInput?: unknown; argumentsRef?: { id: string; bytes: number } };
            }
        ).update;
        expect(update.rawInput).toBeUndefined();
        expect(update.argumentsRef).toBeDefined();

        const payload = await instance.readPayload(update.argumentsRef!);
        expect(JSON.parse(payload.json)).toEqual({ text: "x".repeat(200) });
        await instance.close();
    });

    test("an aborted startup signal rejects before acquiring a runtime", async () => {
        const controller = new AbortController();
        controller.abort(new Error("cancelled"));
        let acquired = 0;
        const instance = createChatInstance(context({
            signal: controller.signal,
            runtime: {
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
            store: { storage: {}, machineId: "test-machine" },
            database: new FakeDatabase(),
            kv: new OwnershipProbe(),
            chatConfig: {
                runtime: {
                    kind: "transport",
                    factory: (path) => {
                        paths.push(path);
                        return transport;
                    },
                },
                session: { path: "/workspace" },
            },
        });
        const chat = manager.createChat({ agent: { cwd: "/" } });

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
            store: { storage: {}, machineId: "test-machine" },
            database: new FakeDatabase(),
            kv: new OwnershipProbe(),
            chatConfig: {
                runtime: { kind: "transport", factory: () => transport },
                session: { path: "/workspace" },
            },
        });
        const chat = manager.createChat({ agent: { cwd: "/" } });
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
