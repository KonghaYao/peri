import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import { ChatManager } from "../src/chat/manager";
import type { ChatManagerInternalOptions } from "../src/chat/manager";
import type { ChatInstanceContext } from "../src/chat/service-context";
import type {
    ChatCreateOptions,
    ChatLifecycleOptions,
    ChatRuntime,
    ChatSessionOptions,
} from "../src/chat/types";
import type { AcpMcpServer } from "../src/contracts/acp";
import type { WorkspaceRef } from "../src/workspace/types";
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

/** 测试用 workspace；路径判断是纯字符串，不依赖真实目录。 */
const ROOT_WORKSPACE: WorkspaceRef = { id: "ws-root", runtimePath: "/" };
function workspaceAt(runtimePath: string, id = `ws:${runtimePath}`): WorkspaceRef {
    return { id, runtimePath };
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
    session?: ChatSessionOptions;
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
                ? { session: {} }
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
            session: {},
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
                session: {},
            },
            wasmRuntimeStarter: starter,
        });

        const first = manager.createChat({
            workspace: workspaceAt("/workspace-a", "ws-a"),
            agent: {},
        });
        const second = manager.createChat({
            workspace: workspaceAt("/workspace-b", "ws-b"),
            agent: {},
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
            workspace: workspaceAt("/first", "ws-first"),
            agent: { settings: { chat: 1 } },
        });
        const second = manager.createChat({
            workspace: workspaceAt("/second", "ws-second"),
            agent: { settings: { chat: 2 } },
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
                workspace: ROOT_WORKSPACE,
                agent: { storage: { url: "libsql://other" } },
            }),
        ).toThrow(/agent\.storage is Manager-scoped/);
        expect(() =>
            manager.createChat({
                workspace: ROOT_WORKSPACE,
                agent: { machineId: "other" },
            }),
        ).toThrow(/agent\.machineId is Manager-scoped/);
        // 保留键检查先于 JSON 校验：非 JSON 值也按越权拒绝。
        expect(() =>
            manager.createChat({
                workspace: ROOT_WORKSPACE,
                agent: { storage: undefined },
            }),
        ).toThrow(/agent\.storage is Manager-scoped/);
        expect(() =>
            manager.createChat({
                workspace: ROOT_WORKSPACE,
                agent: {
                    storage: { url: "libsql://shared" },
                    machineId: "machine-1",
                },
            }),
        ).toThrow(TypeError);
        expect(() =>
            manager.createChat({ workspace: ROOT_WORKSPACE, agent: {} }),
        ).not.toThrow();
    });

    test("createChat requires a workspace and a JSON-safe agent object", () => {
        const manager = createManager();
        const create = (options: unknown): (() => unknown) =>
            () => manager.createChat(options as ChatCreateOptions);

        expect(create(undefined)).toThrow(TypeError);
        expect(create(undefined)).toThrow(/options object/);
        expect(create({})).toThrow(/workspace/);
        expect(create({ workspace: undefined })).toThrow(/workspace/);
        expect(create({ workspace: { id: "ws" } })).toThrow(
            /runtimePath/,
        );
        expect(create({ workspace: ROOT_WORKSPACE, agent: undefined })).toThrow(
            /JSON object/,
        );
        expect(create({ workspace: ROOT_WORKSPACE, agent: null })).toThrow(
            /JSON object/,
        );
        expect(create({ workspace: ROOT_WORKSPACE, agent: [] })).toThrow(
            /JSON object/,
        );
        expect(create({ workspace: ROOT_WORKSPACE, agent: "cwd" })).toThrow(
            /JSON object/,
        );
        expect(
            create({ workspace: ROOT_WORKSPACE, agent: { cwd: "" } }),
        ).toThrow(/agent\.cwd/);
        expect(
            create({ workspace: ROOT_WORKSPACE, agent: { cwd: 1 } }),
        ).toThrow(/agent\.cwd/);
        expect(
            create({
                workspace: ROOT_WORKSPACE,
                agent: { settings: () => {} },
            }),
        ).toThrow(/JSON-safe agent/);
        expect(create({ workspace: ROOT_WORKSPACE })).toThrow(/JSON object/);
        expect(
            create({ workspace: ROOT_WORKSPACE, agent: {} }),
        ).not.toThrow();
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
                session: {},
            },
        });
        const chat = manager.createChat({
            workspace: workspaceAt("/workspace", "ws-factory"),
            agent: { settings: { chat: true } },
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
            workspace: ROOT_WORKSPACE,
            agent: {},
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
                session: {},
            },
            wasmRuntimeStarter: starter,
        });
        const chat = manager.createChat({
            workspace: workspaceAt("/workspace", "ws-env"),
            agent: {},
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
        const defaultInit: ChatSessionOptions = {
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
        const first = manager.createChat({ workspace: ROOT_WORKSPACE, agent: {} });
        const second = manager.createChat({ workspace: ROOT_WORKSPACE, agent: {} });
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
        const { manager, database, ownership } = captureManager();
        database.seed(directoryEntry("session-outside", "/other"));
        const chat = manager.createChat({
            workspace: workspaceAt("/workspace", "ws-guard"),
            agent: {},
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
        const { manager, database } = captureManager();
        database.seed(directoryEntry("session-root", "/workspace/"));
        database.seed(directoryEntry("session-child", "/workspace/sub"));
        const chat = manager.createChat({
            workspace: workspaceAt("/workspace/", "ws-descendants"),
            agent: {},
        });

        await bindSession(chat, "session-child");
        expect(chat.phase).toBe("ready");
        expect(chat.sessionId).toBe("session-child");

        const second = manager.createChat({
            workspace: workspaceAt("/workspace", "ws-root-session"),
            sessionPath: "sub",
            agent: {},
        });
        await bindSession(second, "session-root");
        expect(second.sessionId).toBe("session-root");
    });

    test("rejects a persisted cwd that cannot be compared instead of guessing", async () => {
        const { manager, database } = captureManager();
        database.seed(directoryEntry("relative", "relative/session"));
        const chat = manager.createChat({
            workspace: workspaceAt("/workspace", "ws-relative"),
            agent: {},
        });

        await expect(bindSession(chat, "relative")).rejects.toMatchObject({
            name: "SessionWorkspaceMismatchError",
            code: "workspace_mismatch",
        });
        expect(chat.phase).toBe("empty");

        // workspace.runtimePath 本身必须是绝对路径。
        expect(() =>
            manager.createChat({
                workspace: { id: "ws-bad", runtimePath: "relative-workspace" },
            }),
        ).toThrow(/workspace\.runtimePath must be an absolute path/);
    });

    test("uses the workspaceId map before the path fallback", async () => {
        const { manager } = captureManager();
        const owner = manager.createChat({
            workspace: workspaceAt("/workspace", "ws-owner"),
            agent: {},
        });
        const ready = await owner.execute(ChatUserAction.createNewSession({}));

        const other = manager.createChat({
            workspace: workspaceAt("/other", "ws-other"),
            agent: {},
        });
        await expect(
            bindSession(other, ready.sessionId),
        ).rejects.toMatchObject({
            name: "SessionWorkspaceConflictError",
            code: "workspace_mismatch",
            disposition: "rejected",
            workspaceId: "ws-other",
            sessionWorkspaceId: "ws-owner",
        });
        expect(other.phase).toBe("empty");
        // 同一 workspace 的第二个 Chat 在 ownership 层冲突，而不是 workspace。
        const sameWorkspace = manager.createChat({
            workspace: workspaceAt("/workspace", "ws-owner"),
            agent: {},
        });
        await expect(
            bindSession(sameWorkspace, ready.sessionId),
        ).rejects.toMatchObject({ code: "binding_conflict" });
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
            workspace: ROOT_WORKSPACE,
            agent: {},
            payloadBudgets: budgets,
        });
        const withoutBudgets = manager.createChat({ workspace: ROOT_WORKSPACE, agent: {} });
        await bindNew(withBudgets);
        await bindNew(withoutBudgets, "create-2");

        expect(contexts[0]!.payloadBudgets).toEqual(budgets);
        expect(contexts[0]!.payloadBudgets).not.toBe(budgets);
        expect(contexts[1]!.payloadBudgets).toBeUndefined();
    });
});
