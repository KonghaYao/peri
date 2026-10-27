import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import { ChatManager } from "../src/chat/manager";
import type { ChatInstanceContext } from "../src/chat/service-context";
import type { ChatSessionInit } from "../src/chat/types";
import { FakeDatabase, FakePort } from "./chat-fixtures";
import { OwnershipProbe } from "./ownership-probe";

function setup(options: { defaultSessionInit?: ChatSessionInit } = {}): {
    manager: ChatManager;
    contexts: ChatInstanceContext[];
} {
    const contexts: ChatInstanceContext[] = [];
    const manager = new ChatManager({
        agentConfig: {},
        database: new FakeDatabase(),
        kv: new OwnershipProbe(),
        chatConfig: {
            runtime: {
                kind: "transport",
                factory: () => {
                    throw new Error("ACP transport must not start");
                },
            },
            ...(options.defaultSessionInit === undefined
                ? {}
                : { sessionInit: options.defaultSessionInit }),
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

const defaultInit: ChatSessionInit = {
    path: "/default",
    instructions: "default instructions",
    mcpServers: {
        default: { type: "http", url: "https://example.test/default" },
    },
};

describe("ChatManager createChat sessionInit", () => {
    test("each chat keeps its own sessionInit without leaking the default", async () => {
        const { manager, contexts } = setup({ defaultSessionInit: defaultInit });
        const first = manager.createChat({
            sessionInit: {
                path: "/first",
                instructions: "first instructions",
                mcpServers: {
                    first: { type: "http", url: "https://example.test/first" },
                },
            },
        });
        const second = manager.createChat({
            sessionInit: {
                path: "/second",
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
        expect(contexts[0].sessionInit).not.toBe(contexts[1].sessionInit);
        expect(contexts[0].sessionInit).not.toEqual(defaultInit);
    });

    test("omitting sessionInit falls back to the Manager default", async () => {
        const { manager, contexts } = setup({ defaultSessionInit: defaultInit });
        const first = manager.createChat();
        const second = manager.createChat({ path: "/handle" });

        await bindNew(first);
        await bindNew(second, "create-2");

        expect(contexts[0].sessionInit).toEqual(defaultInit);
        expect(contexts[1].sessionInit).toEqual(defaultInit);
        expect(contexts[0].sessionInit).not.toBe(contexts[1].sessionInit);
        expect(contexts[0].path).toBe("/default");
        expect(contexts[1].path).toBe("/handle");
    });

    test("an explicit sessionInit replaces the default instead of merging fields", async () => {
        const { manager, contexts } = setup({ defaultSessionInit: defaultInit });
        const chat = manager.createChat({
            path: "/handle",
            sessionInit: { path: "/init" },
        });

        await bindNew(chat);

        expect(contexts[0].sessionInit).toEqual({ path: "/init" });
        expect(contexts[0].sessionInit.instructions).toBeUndefined();
        expect(contexts[0].sessionInit.mcpServers).toBeUndefined();
    });

    test("an explicit empty sessionInit is not replaced by the default", async () => {
        const { manager, contexts } = setup({ defaultSessionInit: defaultInit });
        const chat = manager.createChat({ path: "/empty", sessionInit: {} });

        await bindNew(chat);

        expect(contexts[0].sessionInit).toEqual({});
    });

    test("handle path takes precedence over sessionInit.path", async () => {
        const { manager, contexts } = setup();
        const chat = manager.createChat({
            path: "/handle",
            sessionInit: { path: "/init", instructions: "per chat" },
        });

        await bindNew(chat);

        expect(contexts[0].path).toBe("/handle");
        expect(contexts[0].sessionInit.path).toBe("/init");
        expect(contexts[0].sessionInit.instructions).toBe("per chat");
    });

    test("without any sessionInit the chat receives an empty object", async () => {
        const { manager, contexts } = setup();
        const chat = manager.createChat({ path: "/only-path" });

        await bindNew(chat);

        expect(contexts[0].sessionInit).toEqual({});
        expect(contexts[0].path).toBe("/only-path");
    });

    test("the Manager copies its default at construction and per chat at creation", async () => {
        const callerDefault: ChatSessionInit = {
            path: "/default",
            instructions: "original",
        };
        const { manager, contexts } = setup({ defaultSessionInit: callerDefault });
        callerDefault.instructions = "mutated caller default";

        const first = manager.createChat();
        await bindNew(first);
        expect(contexts[0].sessionInit.instructions).toBe("original");

        contexts[0].sessionInit.instructions = "mutated chat";
        contexts[0].sessionInit.mcpServers = {
            hijacked: { type: "http", url: "https://example.test/hijacked" },
        };

        const second = manager.createChat();
        await bindNew(second, "create-2");
        expect(contexts[1].sessionInit).toEqual({
            path: "/default",
            instructions: "original",
        });
        expect(contexts[1].sessionInit).not.toBe(contexts[0].sessionInit);
    });
});
