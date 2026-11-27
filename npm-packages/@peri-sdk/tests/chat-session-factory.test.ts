import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import { ChatClosedError } from "../src/chat/errors";
import {
    createChatSession,
    createChatSessionInternal,
    readChatSessionState,
} from "../src/chat/session-factory";
import type { CreateChatSessionInput } from "../src/chat/session-factory";
import type { ChatInstanceContext } from "../src/chat/service-context";
import type { ChatConfig } from "../src/chat/types";
import { FakeDatabase, FakePort } from "./chat-fixtures";
import { TestTransport } from "./chat-transport-fixture";
import { OwnershipProbe } from "./ownership-probe";

const TEST_WORKSPACE = { id: "ws-session", runtimePath: "/workspace" };

function throwTransport(): never {
    throw new Error("ACP transport must not start");
}

function transportConfig(): ChatConfig {
    return { runtime: { kind: "transport", factory: throwTransport } };
}

function input(
    overrides: Partial<CreateChatSessionInput> = {},
): CreateChatSessionInput {
    return {
        target: {
            sessionId: "session-1",
            workspace: TEST_WORKSPACE,
            agent: {},
            storage: { url: "postgresql://session" },
        },
        database: new FakeDatabase(),
        ownership: new OwnershipProbe(),
        machineId: "machine-1",
        chatConfig: transportConfig(),
        ...overrides,
    };
}

describe("createChatSession", () => {
    test("assembles a handle that can execute one action end to end", async () => {
        const ownership = new OwnershipProbe();
        const database = new FakeDatabase();
        const transport = new TestTransport(undefined, "session-new");
        const paths: string[] = [];
        const chat = await createChatSession(input({
            ownership,
            database,
            chatConfig: {
                runtime: {
                    kind: "transport",
                    factory: (path) => {
                        paths.push(path);
                        return transport;
                    },
                },
                session: { instructions: "be brief" },
            },
        }));

        // 装配只建立句柄：不启动 ACP、不取得 ownership。
        expect(paths).toHaveLength(0);
        expect(ownership.size).toBe(0);
        expect(chat.phase).toBe("empty");
        expect(database.getCalls).toHaveLength(0);

        const ready = await chat.execute(
            ChatUserAction.createNewSession({ requestId: "req-1" }),
        );

        expect(ready.sessionId).toBe("session-new");
        expect(chat.phase).toBe("ready");
        expect(paths).toEqual(["/workspace"]);
        expect(ownership.has("session-new")).toBe(true);
        expect(
            transport.requests.map((request) => request.method),
        ).toContain("session/new");
        // 句柄身份与会话身份分离：不会冒充 Session。
        expect(chat.id).not.toBe("session-1");
        expect(readChatSessionState(chat)?.sessionId).toBe("session-1");

        await chat.close();
        expect(transport.closeCalls).toBe(1);
        expect(ownership.size).toBe(0);
    });

    test("a failed assembly leaves no half-open resources and does not block a retry", async () => {
        const ownership = new OwnershipProbe();
        let transports = 0;
        const bad = input({
            ownership,
            chatConfig: {
                runtime: {
                    kind: "transport",
                    factory: () => {
                        transports += 1;
                        return new TestTransport();
                    },
                },
            },
        });

        // agent.cwd 与 workspace 不一致：校验在构造之前失败，错误原样上抛。
        await expect(
            createChatSession({
                ...bad,
                target: { ...bad.target, agent: { cwd: "/elsewhere" } },
            }),
        ).rejects.toThrow(/workspace_mismatch|cwd/u);

        expect(ownership.size).toBe(0);
        expect(transports).toBe(0);

        // 同一 ownership/database 仍可用于下一次装配。
        const chat = await createChatSession(bad);
        await chat.execute(
            ChatUserAction.createNewSession({ requestId: "req-retry" }),
        );
        expect(transports).toBe(1);
        expect(ownership.has("session-new")).toBe(true);
        await chat.close();
    });

    test("rejects invalid boundaries without touching ownership or transports", async () => {
        const ownership = new OwnershipProbe();
        const base = input({ ownership });
        const cases: Array<Partial<CreateChatSessionInput>> = [
            { target: { ...base.target, sessionId: "" } },
            { target: { ...base.target, storage: { url: "" } } },
            {
                target: {
                    ...base.target,
                    storage: { url: "postgresql://x", httpEndpoint: 1 as never },
                },
            },
            { chatConfig: { runtime: { kind: "nope" } } as never },
            { chatConfig: undefined as never },
        ];

        for (const override of cases) {
            await expect(
                createChatSession({ ...base, ...override }),
            ).rejects.toBeInstanceOf(Error);
        }
        expect(ownership.size).toBe(0);

        // 合法输入仍可装配，说明上述失败没有污染共享端口。
        const chat = await createChatSession(base);
        expect(chat.phase).toBe("empty");
        await chat.close();
    });

    test("an unconfirmed close keeps the closed flag false for retry", async () => {
        const transport = new TestTransport();
        transport.closeError = new Error("transport close failed");
        const chat = await createChatSession(input({
            chatConfig: {
                runtime: { kind: "transport", factory: () => transport },
            },
        }));
        await chat.execute(
            ChatUserAction.createNewSession({ requestId: "req-close" }),
        );

        // 关闭未确认时 onClosed 不触发：标记仍为 false，句柄可重试。
        await expect(chat.close()).rejects.toBeInstanceOf(Error);
        expect(readChatSessionState(chat)?.closed).toBe(false);
        expect(chat.phase).toBe("cleanup-pending");

        transport.closeError = undefined;
        await chat.close();
        expect(readChatSessionState(chat)?.closed).toBe(true);
        expect(chat.phase).toBe("closed");
    });

    test("close flips the closed flag and blocks further actions", async () => {
        const chat = await createChatSession(input());
        expect(readChatSessionState(chat)).toEqual({
            sessionId: "session-1",
            closed: false,
        });

        await chat.close();

        expect(readChatSessionState(chat)?.closed).toBe(true);
        expect(chat.phase).toBe("closed");
        // 关闭后准入同步封闭：`createNewSession` 在返回 Promise 前抛出。
        expect(() =>
            chat.execute(ChatUserAction.createNewSession({ requestId: "late" })),
        ).toThrow(ChatClosedError);
    });
});

describe("createChatSessionInternal seams", () => {
    test("injected instance factory receives the session agentConfig", async () => {
        const contexts: ChatInstanceContext[] = [];
        const chat = await createChatSessionInternal({
            ...input({
                target: {
                    ...input().target,
                    agent: { settings: { chat: true } },
                    storage: { url: "postgresql://session", httpEndpoint: "https://proxy.example.org/sql" },
                },
            }),
            instanceFactory: (context) => {
                contexts.push(context);
                const port = new FakePort();
                port.bindNewSessionId = "session-internal";
                return port;
            },
        });

        const ready = await chat.execute(
            ChatUserAction.createNewSession({ requestId: "internal-1" }),
        );

        expect(ready.sessionId).toBe("session-internal");
        expect(contexts).toHaveLength(1);
        expect(contexts[0]!.agentConfig).toMatchObject({
            cwd: "/workspace",
            settings: { chat: true },
            storage: { url: "postgresql://session", httpEndpoint: "https://proxy.example.org/sql" },
            machineId: "machine-1",
        });
        expect(readChatSessionState(chat)?.sessionId).toBe("session-1");

        await chat.close();
    });

    test("transport runtime assembles without a live runtime", async () => {
        const chat = await createChatSessionInternal(input());
        // transport 句柄在首次激活前不建立连接；装配后仍是未绑定状态。
        expect(chat.phase).toBe("empty");
        await chat.close();
        expect(chat.phase).toBe("closed");
    });
});
