import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import { ChatManager } from "../src/chat/manager";
import type { ChatManagerInternalOptions } from "../src/chat/manager";
import type { ChatInstanceContext } from "../src/chat/service-context";
import type {
    ChatCreateOptions,
    ChatLifecycleOptions,
    ChatRuntime,
    ChatSessionInit,
} from "../src/chat/types";
import type { AcpMcpServer } from "../src/contracts/acp";
import type { WasmAcpTransport } from "../src/wasm/transport";
import type {
    WasmRuntimeStarter,
    WasmRuntimeStarterOptions,
} from "../src/wasm/runtime-pool";
import { FakeDatabase, FakePort, directoryEntry } from "./chat-fixtures";
import { TestTransport } from "./chat-transport-fixture";
import { OwnershipProbe } from "./ownership-probe";

function throwTransport(): never {
    throw new Error("ACP transport must not start");
}

interface CaptureSetup {
    readonly manager: ChatManager;
    readonly contexts: ChatInstanceContext[];
    readonly database: FakeDatabase;
    readonly ownership: OwnershipProbe;
}

function captureManager(options: {
    storage?: unknown;
    machineId?: string;
    session?: ChatSessionInit;
    runtime?: ChatRuntime;
    lifecycle?: ChatLifecycleOptions;
} = {}): CaptureSetup {
    const contexts: ChatInstanceContext[] = [];
    const database = new FakeDatabase();
    const ownership = new OwnershipProbe();
    const managerOptions: ChatManagerInternalOptions = {
        store: {
            storage: options.storage ?? { url: "libsql://shared" },
            machineId: options.machineId ?? "machine-1",
        },
        database,
        kv: ownership,
        chatConfig: {
            runtime: options.runtime ?? {
                kind: "transport",
                factory: throwTransport,
            },
            ...(options.session === undefined
                ? { session: { path: "/workspace" } }
                : { session: options.session }),
            ...(options.lifecycle === undefined
                ? {}
                : { lifecycle: options.lifecycle }),
        },
        instanceFactory: (context) => {
            contexts.push(context);
            const port = new FakePort();
            port.bindNewSessionId = `session-${contexts.length}`;
            return port;
        },
    };
    return {
        manager: new ChatManager(managerOptions),
        contexts,
        database,
        ownership,
    };
}

function createManager(
    store: { storage?: unknown; machineId?: string } = {},
): ChatManager {
    return new ChatManager({
        store: {
            storage: store.storage ?? {},
            machineId: store.machineId ?? "machine-1",
        },
        database: new FakeDatabase(),
        kv: new OwnershipProbe(),
        chatConfig: {
            runtime: { kind: "transport", factory: throwTransport },
            session: { path: "/workspace" },
        },
        instanceFactory: () => new FakePort(),
    });
}

async function bindNew(
    chat: ReturnType<ChatManager["createChat"]>,
    requestId = "create-1",
): Promise<void> {
    await chat.execute(ChatUserAction.createNewSession({ requestId }));
}

async function bindSession(
    chat: ReturnType<ChatManager["createChat"]>,
    sessionId: string,
    requestId = `load-${sessionId}`,
): Promise<void> {
    await chat.execute(ChatUserAction.changeSessionTo(sessionId, { requestId }));
}

function configOf(context: ChatInstanceContext): Record<string, unknown> {
    return context.agentConfig as Record<string, unknown>;
}

describe("per-chat agent", () => {
    test("one Manager starts two chats with different cwd configs and the shared store", async () => {
        const starts: WasmRuntimeStarterOptions[] = [];
        const starter: WasmRuntimeStarter = async (options) => {
            starts.push(options);
            return new TestTransport(
                undefined,
                `session-${starts.length}`,
            ) as unknown as WasmAcpTransport;
        };
        const manager = new ChatManager({
            store: {
                storage: { url: "libsql://shared" },
                machineId: "machine-1",
            },
            database: new FakeDatabase(),
            kv: new OwnershipProbe(),
            chatConfig: {
                runtime: { kind: "wasm" },
                session: { path: "/workspace" },
            },
            wasmRuntimeStarter: starter,
        });

        const first = manager.createChat({
            path: "/workspace-a",
            agent: { cwd: "/workspace-a" },
        });
        const second = manager.createChat({
            path: "/workspace-b",
            agent: { cwd: "/workspace-b" },
        });
        await bindNew(first, "create-a");
        await bindNew(second, "create-b");

        expect(starts).toHaveLength(2);
        const firstConfig = JSON.parse(starts[0]!.configJson!);
        const secondConfig = JSON.parse(starts[1]!.configJson!);
        expect(firstConfig).toEqual({
            cwd: "/workspace-a",
            storage: { url: "libsql://shared" },
            machineId: "machine-1",
        });
        expect(secondConfig).toEqual({
            cwd: "/workspace-b",
            storage: { url: "libsql://shared" },
            machineId: "machine-1",
        });
        expect(firstConfig.storage).not.toBe(secondConfig.storage);
        expect(starts[0]!.configJson).not.toBe(starts[1]!.configJson);
        await manager.shutdown();
    });

    test("each chat gets an independent frozen config with the Manager store injected", async () => {
        const sharedStorage: { nested: { value: string } } = {
            nested: { value: "original" },
        };
        const { manager, contexts } = captureManager({
            storage: sharedStorage,
        });
        sharedStorage.nested.value = "mutated after construction";

        const first = manager.createChat({
            path: "/first",
            agent: { cwd: "/first", settings: { chat: 1 } },
        });
        const second = manager.createChat({
            path: "/second",
            agent: { cwd: "/second", settings: { chat: 2 } },
        });
        await bindNew(first);
        await bindNew(second, "create-2");

        expect(configOf(contexts[0]!)).toEqual({
            cwd: "/first",
            settings: { chat: 1 },
            storage: { nested: { value: "original" } },
            machineId: "machine-1",
        });
        expect(configOf(contexts[1]!)).toEqual({
            cwd: "/second",
            settings: { chat: 2 },
            storage: { nested: { value: "original" } },
            machineId: "machine-1",
        });
        expect(contexts[0]!.agentConfig).not.toBe(contexts[1]!.agentConfig);
        expect(configOf(contexts[0]!).storage).not.toBe(
            configOf(contexts[1]!).storage,
        );
        expect(Object.isFrozen(contexts[0]!.agentConfig)).toBe(true);
        expect(Object.isFrozen(configOf(contexts[0]!).storage)).toBe(true);
        expect(Object.isFrozen(configOf(contexts[0]!).settings)).toBe(true);
        expect(JSON.parse(contexts[0]!.agentConfigJson)).toEqual(
            configOf(contexts[0]!),
        );
    });

    test("per-chat agent cannot declare storage or machineId, even with Manager values", () => {
        const manager = createManager({
            storage: { url: "libsql://shared" },
            machineId: "machine-1",
        });

        expect(() =>
            manager.createChat({
                agent: { cwd: "/", storage: { url: "libsql://other" } },
            }),
        ).toThrow(/agent\.storage is Manager-scoped/);
        expect(() =>
            manager.createChat({
                agent: { cwd: "/", machineId: "other" },
            }),
        ).toThrow(/agent\.machineId is Manager-scoped/);
        // 保留键检查先于 JSON 校验：非 JSON 值也按越权拒绝。
        expect(() =>
            manager.createChat({
                agent: { cwd: "/", storage: undefined },
            }),
        ).toThrow(/agent\.storage is Manager-scoped/);
        expect(() =>
            manager.createChat({
                agent: {
                    cwd: "/",
                    storage: { url: "libsql://shared" },
                    machineId: "machine-1",
                },
            }),
        ).toThrow(TypeError);
        expect(() =>
            manager.createChat({ agent: { cwd: "/" } }),
        ).not.toThrow();
    });

    test("createChat requires a JSON object with a non-empty cwd", () => {
        const manager = createManager();
        const create = (options: unknown): (() => unknown) =>
            () => manager.createChat(options as ChatCreateOptions);

        expect(create(undefined)).toThrow(TypeError);
        expect(create(undefined)).toThrow(/options object/);
        expect(create({})).toThrow(TypeError);
        expect(create({})).toThrow(/JSON object/);
        expect(create({ agent: undefined })).toThrow(TypeError);
        expect(create({ agent: undefined })).toThrow(/JSON object/);
        expect(create({ agent: null })).toThrow(/JSON object/);
        expect(create({ agent: [] })).toThrow(/JSON object/);
        expect(create({ agent: "cwd" })).toThrow(/JSON object/);
        expect(create({ agent: { settings: {} } })).toThrow(
            /agent\.cwd/,
        );
        expect(create({ agent: { cwd: "" } })).toThrow(
            /agent\.cwd/,
        );
        expect(create({ agent: { cwd: 1 } })).toThrow(
            /agent\.cwd/,
        );
        expect(create({ agent: { cwd: "/", settings: () => {} } })).toThrow(
            /JSON-safe agent/,
        );
        expect(create({ agent: { cwd: "/" } })).not.toThrow();
    });

    test("the Manager requires store with a non-empty machineId and JSON-safe storage", () => {
        const options = (store: unknown): ChatManagerInternalOptions => ({
            store: store as ChatManagerInternalOptions["store"],
            database: new FakeDatabase(),
            kv: new OwnershipProbe(),
            chatConfig: {
                runtime: { kind: "transport", factory: throwTransport },
            },
        });

        expect(() => new ChatManager(options(undefined))).toThrow(
            /store: \{ storage, machineId \}/,
        );
        expect(() =>
            new ChatManager(options({ storage: {} })),
        ).toThrow(/machineId/);
        expect(() =>
            new ChatManager(options({ storage: {}, machineId: "" })),
        ).toThrow(/machineId/);
        expect(() =>
            new ChatManager(options({ storage: {}, machineId: 1 })),
        ).toThrow(/machineId/);
        expect(() =>
            new ChatManager(options({ machineId: "m" })),
        ).toThrow(/store\.storage/);
        expect(() =>
            new ChatManager(options({ storage: () => {}, machineId: "m" })),
        ).toThrow(/store\.storage must be JSON data/);
    });

    test("transport factory receives chatId and the per-chat config with the Manager store", async () => {
        const calls: Array<{
            path: string;
            chatId?: string;
            agentConfig?: unknown;
            signal?: AbortSignal;
        }> = [];
        const manager = new ChatManager({
            store: {
                storage: { url: "libsql://shared" },
                machineId: "machine-1",
            },
            database: new FakeDatabase(),
            kv: new OwnershipProbe(),
            chatConfig: {
                runtime: {
                    kind: "transport",
                    factory: (path, options) => {
                        calls.push({ path, ...options });
                        return new TestTransport(undefined, "session-factory");
                    },
                },
                session: { path: "/workspace" },
            },
        });
        const chat = manager.createChat({
            agent: { cwd: "/workspace", settings: { chat: true } },
        });
        await bindNew(chat);

        expect(calls).toHaveLength(1);
        expect(calls[0]!.path).toBe("/workspace");
        expect(calls[0]!.chatId).toBe(chat.id);
        expect(calls[0]!.agentConfig).toEqual({
            cwd: "/workspace",
            settings: { chat: true },
            storage: { url: "libsql://shared" },
            machineId: "machine-1",
        });
        expect(calls[0]!.signal).toBeDefined();
        await manager.shutdown();
    });
});

describe("per-chat lifecycle and runtime", () => {
    test("lifecycle overrides merge per chat while shutdown stays Manager-scoped", async () => {
        const { manager, contexts } = captureManager({
            lifecycle: {
                closeDrainTimeoutMs: 1000,
                startupSettleTimeoutMs: 1500,
                activationCloseTimeoutMs: 4000,
                shutdownChatTimeoutMs: 2000,
            },
        });
        const chat = manager.createChat({
            agent: { cwd: "/" },
            lifecycle: {
                closeDrainTimeoutMs: 300,
                activationCloseTimeoutMs: 250,
                shutdownChatTimeoutMs: 60_000,
            },
        });
        await bindNew(chat);

        expect(contexts[0]!.lifecycle).toEqual({
            closeDrainTimeoutMs: 300,
            startupSettleTimeoutMs: 1500,
            activationCloseTimeoutMs: 250,
            shutdownChatTimeoutMs: 2000,
        });
    });

    test("per-chat env merges over the Manager env and diagnostics reach the starter", async () => {
        const starts: WasmRuntimeStarterOptions[] = [];
        const starter: WasmRuntimeStarter = async (options) => {
            starts.push(options);
            return new TestTransport(
                undefined,
                `session-${starts.length}`,
            ) as unknown as WasmAcpTransport;
        };
        const managerDiagnostics: string[] = [];
        const chatDiagnostics: string[] = [];
        const manager = new ChatManager({
            store: { storage: {}, machineId: "machine-1" },
            database: new FakeDatabase(),
            kv: new OwnershipProbe(),
            chatConfig: {
                runtime: {
                    kind: "wasm",
                    env: { A: "1", SHARED: "manager" },
                    onDiagnostic: (kind) => managerDiagnostics.push(kind),
                },
                session: { path: "/workspace" },
            },
            wasmRuntimeStarter: starter,
        });
        const chat = manager.createChat({
            path: "/workspace",
            agent: { cwd: "/workspace" },
            runtime: {
                env: { SHARED: "chat", C: "3" },
                onDiagnostic: (kind) => chatDiagnostics.push(kind),
            },
        });
        await bindNew(chat);

        expect(starts[0]!.env).toEqual({ A: "1", SHARED: "chat", C: "3" });
        starts[0]!.onDiagnostic?.("startup", new Error("boom"));
        expect(chatDiagnostics).toEqual(["startup"]);
        expect(managerDiagnostics).toEqual([]);
        await manager.shutdown();
    });
});

describe("session isolation", () => {
    test("nested MCP descriptors are copied per chat while acp instances stay referenced", async () => {
        const acpServer = { connect: async (): Promise<void> => {} };
        const defaultInit: ChatSessionInit = {
            path: "/workspace",
            mcpServers: {
                http: {
                    type: "http",
                    url: "https://example.test/http",
                    headers: { Authorization: "Bearer default" },
                },
                stdio: {
                    type: "stdio",
                    command: "node",
                    args: ["server.js"],
                    env: { TOKEN: "default" },
                },
                acp: { type: "acp", server: acpServer, serverId: "acp-default" },
            },
        };
        const { manager, contexts } = captureManager({
            session: defaultInit,
        });
        const first = manager.createChat({ agent: { cwd: "/" } });
        const second = manager.createChat({ agent: { cwd: "/" } });
        await bindNew(first);
        await bindNew(second, "create-2");

        const firstServers = contexts[0]!.sessionInit.mcpServers!;
        const secondServers = contexts[1]!.sessionInit.mcpServers!;
        expect(firstServers).not.toBe(secondServers);

        const firstHttp = firstServers.http as AcpMcpServer & { type: "http" };
        const secondHttp = secondServers.http as AcpMcpServer & { type: "http" };
        firstHttp.headers!.Authorization = "Bearer mutated";
        expect(secondHttp.headers).toEqual({ Authorization: "Bearer default" });

        const firstStdio = firstServers.stdio as AcpMcpServer & { type: "stdio" };
        const secondStdio = secondServers.stdio as AcpMcpServer & { type: "stdio" };
        firstStdio.args!.push("mutated.js");
        firstStdio.env!.TOKEN = "mutated";
        expect(secondStdio.args).toEqual(["server.js"]);
        expect(secondStdio.env).toEqual({ TOKEN: "default" });

        const firstAcp = firstServers.acp as AcpMcpServer & { type: "acp" };
        const secondAcp = secondServers.acp as AcpMcpServer & { type: "acp" };
        expect(firstAcp).not.toBe(secondAcp);
        expect(firstAcp.server).toBe(acpServer);
        expect(secondAcp.server).toBe(acpServer);
        expect(defaultInit.mcpServers!.http).not.toBe(
            contexts[1]!.sessionInit.mcpServers!.http,
        );
    });
});

describe("change_session workspace guard", () => {
    test("rejects a persisted cwd outside the chat workspace and keeps the handle empty", async () => {
        const { manager, database, ownership } = captureManager({
            session: { path: "/workspace" },
        });
        database.seed(directoryEntry("session-outside", "/other"));
        const chat = manager.createChat({
            path: "/workspace",
            agent: { cwd: "/workspace" },
        });

        await expect(
            bindSession(chat, "session-outside"),
        ).rejects.toMatchObject({
            name: "SessionWorkspaceMismatchError",
            code: "workspace_mismatch",
            disposition: "rejected",
            sessionPath: "/other",
            workspaceRoot: "/workspace",
        });
        expect(chat.phase).toBe("empty");
        expect(chat.binding).toBeNull();
        expect(ownership.size).toBe(0);
        expect(manager.findChatBySession("session-outside")).toBeUndefined();
    });

    test("allows the workspace root and its descendants", async () => {
        const { manager, database } = captureManager({
            session: { path: "/workspace" },
        });
        database.seed(directoryEntry("session-child", "/workspace/sub"));
        const chat = manager.createChat({
            path: "/workspace/",
            agent: { cwd: "/workspace" },
        });

        await bindSession(chat, "session-child");
        expect(chat.phase).toBe("ready");
        expect(chat.sessionId).toBe("session-child");
    });

    test("uses agent.cwd as the root and skips the guard when paths cannot be compared", async () => {
        const { manager, database } = captureManager({ session: {} });
        database.seed(directoryEntry("inside", "/root/sub"));
        database.seed(directoryEntry("outside", "/elsewhere"));
        database.seed(directoryEntry("unscoped", "/anywhere"));

        const scoped = manager.createChat({ agent: { cwd: "/root" } });
        await bindSession(scoped, "inside");
        expect(scoped.sessionId).toBe("inside");

        const rejected = manager.createChat({ agent: { cwd: "/root" } });
        await expect(bindSession(rejected, "outside")).rejects.toMatchObject({
            code: "workspace_mismatch",
        });

        // 相对路径无法确定基准：跳过校验而不是猜测。
        const unscoped = manager.createChat({
            agent: { cwd: "relative-workspace" },
        });
        await bindSession(unscoped, "unscoped");
        expect(unscoped.sessionId).toBe("unscoped");
    });

    test("rejects new session paths outside agent.cwd including prefix lookalikes", async () => {
        const { manager } = captureManager({ session: {} });
        const outside = manager.createChat({
            path: "/workspace-other",
            agent: { cwd: "/workspace" },
        });
        await expect(bindNew(outside)).rejects.toMatchObject({
            name: "SessionWorkspaceMismatchError",
            code: "workspace_mismatch",
        });
        expect(outside.phase).toBe("empty");

        const inside = manager.createChat({
            path: "/workspace/",
            agent: { cwd: "/workspace" },
        });
        await bindNew(inside, "create-2");
        expect(inside.phase).toBe("ready");
    });
});

describe("per-chat payload budgets", () => {
    test("createChat passes budgets to the instance context without a Manager default", async () => {
        const budgets = {
            inlineBytes: 128,
            externalBytes: 1024,
            maxPayloadBytes: 4096,
        };
        const { manager, contexts } = captureManager();
        const withBudgets = manager.createChat({
            agent: { cwd: "/" },
            payloadBudgets: budgets,
        });
        const withoutBudgets = manager.createChat({ agent: { cwd: "/" } });
        await bindNew(withBudgets);
        await bindNew(withoutBudgets, "create-2");

        expect(contexts[0]!.payloadBudgets).toEqual(budgets);
        expect(contexts[0]!.payloadBudgets).not.toBe(budgets);
        expect(contexts[1]!.payloadBudgets).toBeUndefined();
    });
});
