import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import { ChatManager } from "../src/chat/manager";
import type { ChatInstanceContext } from "../src/chat/service-context";
import type { ChatSessionOptions } from "../src/chat/types";
import { WorkspaceMismatchError } from "../src/workspace/errors";
import { FakeDatabase, FakePort } from "./chat-fixtures";
import { OwnershipProbe } from "./ownership-probe";

const ROOT_WORKSPACE = { id: "ws-root", runtimePath: "/" };

function setup(options: { defaultSession?: ChatSessionOptions } = {}): {
    manager: ChatManager;
    contexts: ChatInstanceContext[];
} {
    const contexts: ChatInstanceContext[] = [];
    const manager = new ChatManager({
        store: { storage: {}, machineId: "test-machine" },
        database: new FakeDatabase(),
        kv: new OwnershipProbe(),
        chatConfig: {
            runtime: {
                kind: "transport",
                factory: () => {
                    throw new Error("ACP transport must not start");
                },
            },
            ...(options.defaultSession === undefined
                ? {}
                : { session: options.defaultSession }),
        },
        instanceFactory: (context) => {
            contexts.push(context);
            const port = new FakePort();
            port.bindNewSessionId = `session-${contexts.length}`;
            return port;
        },
    });
    return { manager, contexts };
}

async function bindNew(
    chat: ReturnType<ChatManager["createChat"]>,
    requestId = "create-1",
): Promise<void> {
    await chat.execute(ChatUserAction.createNewSession({ requestId }));
}

const defaultInit: ChatSessionOptions = {
    instructions: "default instructions",
    mcpServers: {
        default: { type: "http", url: "https://example.test/default" },
    },
};

describe("ChatManager createChat workspace", () => {
    test("derives each chat session cwd and agent cwd from its own workspace", async () => {
        const { manager, contexts } = setup({ defaultSession: defaultInit });
        const first = manager.createChat({
            workspace: {
                id: "ws-first",
                hostPath: "/host/first",
                runtimePath: "/first/",
            },
            agent: {},
            session: {
                instructions: "first instructions",
                mcpServers: {
                    first: { type: "http", url: "https://example.test/first" },
                },
            },
        });
        const second = manager.createChat({
            workspace: {
                id: "ws-second",
                hostPath: "/host/second",
                runtimePath: "/second",
            },
            agent: {},
            session: {
                instructions: "second instructions",
                mcpServers: {
                    second: { type: "http", url: "https://example.test/second" },
                },
            },
        });

        await bindNew(first);
        await bindNew(second, "create-2");

        expect(contexts[0].sessionInit).toEqual({
            path: "/first",
            instructions: "first instructions",
            mcpServers: {
                first: { type: "http", url: "https://example.test/first" },
            },
        });
        expect(contexts[1].sessionInit).toEqual({
            path: "/second",
            instructions: "second instructions",
            mcpServers: {
                second: { type: "http", url: "https://example.test/second" },
            },
        });
        expect(contexts[0].agentConfig).toMatchObject({
            cwd: "/first",
        });
        expect(contexts[1].agentConfig).toMatchObject({
            cwd: "/second",
        });
        expect(contexts[0].sessionInit).not.toBe(contexts[1].sessionInit);
        expect(contexts[0].sessionInit).not.toEqual(defaultInit);
    });

    test("omitting session falls back to the Manager default but derives the cwd", async () => {
        const { manager, contexts } = setup({ defaultSession: defaultInit });
        const first = manager.createChat({
            workspace: { id: "ws-default", runtimePath: "/default" },
            agent: {},
        });
        const second = manager.createChat({
            workspace: { id: "ws-default", runtimePath: "/default" },
            sessionPath: "handle",
            agent: {},
        });

        await bindNew(first);
        await bindNew(second, "create-2");

        expect(contexts[0].sessionInit).toEqual({
            ...defaultInit,
            path: "/default",
        });
        expect(contexts[1].sessionInit).toEqual({
            ...defaultInit,
            path: "/default/handle",
        });
        expect(contexts[0].sessionInit).not.toBe(contexts[1].sessionInit);
        expect(contexts[0].path).toBe("/default");
        expect(contexts[1].path).toBe("/default/handle");
    });

    test("an explicit session replaces the default instead of merging fields", async () => {
        const { manager, contexts } = setup({ defaultSession: defaultInit });
        const chat = manager.createChat({
            workspace: { id: "ws-init", runtimePath: "/" },
            agent: {},
            session: { instructions: "only instructions" },
        });

        await bindNew(chat);

        expect(contexts[0].sessionInit).toEqual({
            path: "/",
            instructions: "only instructions",
        });
        expect(contexts[0].sessionInit.mcpServers).toBeUndefined();
    });

    test("an explicit empty session is not replaced by the default", async () => {
        const { manager, contexts } = setup({ defaultSession: defaultInit });
        const chat = manager.createChat({
            workspace: { id: "ws-empty", runtimePath: "/empty" },
            agent: {},
            session: {},
        });

        await bindNew(chat);

        expect(contexts[0].sessionInit).toEqual({ path: "/empty" });
    });

    test("sessionPath is resolved relative to the workspace runtimePath", async () => {
        const { manager, contexts } = setup();
        const chat = manager.createChat({
            workspace: { id: "ws-sub", runtimePath: "/workspace" },
            sessionPath: "sub/dir",
            agent: {},
            session: { instructions: "per chat" },
        });

        await bindNew(chat);

        expect(contexts[0].path).toBe("/workspace/sub/dir");
        expect(contexts[0].sessionInit.path).toBe("/workspace/sub/dir");
        expect(contexts[0].sessionInit.instructions).toBe("per chat");
    });

    test("rejects sessionPath that escapes the workspace runtimePath", () => {
        const { manager } = setup();
        const create = (sessionPath: string): (() => unknown) => () =>
            manager.createChat({
                workspace: { id: "ws-escape", runtimePath: "/workspace" },
                sessionPath,
                agent: {},
            });

        expect(create("../other")).toThrow(WorkspaceMismatchError);
        expect(create("../other")).toThrow(/escapes the workspace runtime root/);
        expect(create("/workspace/sub")).toThrow(WorkspaceMismatchError);
        expect(create("sub/../../other")).toThrow(WorkspaceMismatchError);
        expect(create("sub/..")).not.toThrow();
    });

    test("agent.cwd is optional but must equal the workspace runtimePath", () => {
        const { manager } = setup();
        const workspace = { id: "ws-cwd", runtimePath: "/workspace" };

        expect(manager.createChat({ workspace, agent: {} })).toBeDefined();
        expect(
            manager.createChat({ workspace, agent: { cwd: "/workspace/" } }),
        ).toBeDefined();
        expect(() =>
            manager.createChat({ workspace, agent: { cwd: "/other" } }),
        ).toThrow(WorkspaceMismatchError);
        expect(() =>
            manager.createChat({ workspace, agent: { cwd: "relative" } }),
        ).toThrow(WorkspaceMismatchError);
    });

    test("requires an absolute runtimePath and only accepts an absolute hostPath", () => {
        const { manager } = setup();

        expect(() =>
            manager.createChat({ workspace: { id: "ws", runtimePath: "relative" } }),
        ).toThrow(/workspace\.runtimePath must be an absolute path/);
        expect(() =>
            manager.createChat({
                workspace: { id: "", runtimePath: "/workspace" },
            }),
        ).toThrow(/workspace\.id/);
        expect(() =>
            manager.createChat({
                workspace: {
                    id: "ws",
                    hostPath: "relative",
                    runtimePath: "/workspace",
                },
            }),
        ).toThrow(/workspace\.hostPath must be an absolute path/);
        // 远程 sandbox 没有本地宿主路径时可以省略 hostPath。
        expect(
            manager.createChat({
                workspace: { id: "ws", runtimePath: "/workspace" },
                agent: {},
            }),
        ).toBeDefined();
    });

    test("rejects session.path in per-chat session and Manager defaults", () => {
        const { manager } = setup();
        expect(() =>
            manager.createChat({
                workspace: ROOT_WORKSPACE,
                agent: {},
                session: { path: "/ignored" } as unknown as ChatSessionOptions,
            }),
        ).toThrow(/session\.path is not supported/);

        expect(() =>
            new ChatManager({
                store: { storage: {}, machineId: "test-machine" },
                database: new FakeDatabase(),
                kv: new OwnershipProbe(),
                chatConfig: {
                    runtime: { kind: "transport", factory: () => {
                        throw new Error("ACP transport must not start");
                    } },
                    session: { path: "/ignored" } as unknown as ChatSessionOptions,
                },
            }),
        ).toThrow(/session\.path is not supported/);
    });

    test("the Manager copies its default at construction and per chat at creation", async () => {
        const callerDefault: ChatSessionOptions = {
            instructions: "original",
        };
        const { manager, contexts } = setup({ defaultSession: callerDefault });
        callerDefault.instructions = "mutated caller default";

        const first = manager.createChat({ workspace: ROOT_WORKSPACE, agent: {} });
        await bindNew(first);
        expect(contexts[0].sessionInit.instructions).toBe("original");

        contexts[0].sessionInit.instructions = "mutated chat";
        contexts[0].sessionInit.mcpServers = {
            hijacked: { type: "http", url: "https://example.test/hijacked" },
        };

        const second = manager.createChat({ workspace: ROOT_WORKSPACE, agent: {} });
        await bindNew(second, "create-2");
        expect(contexts[1].sessionInit).toEqual({
            path: "/",
            instructions: "original",
        });
        expect(contexts[1].sessionInit).not.toBe(contexts[0].sessionInit);
    });
});
