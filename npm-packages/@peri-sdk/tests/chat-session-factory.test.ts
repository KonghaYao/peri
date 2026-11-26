import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import { ChatClosedError } from "../src/chat/errors";
import {
    createChatSession,
    createChatSessionInternal,
    readChatSessionState,
} from "../src/chat/session-factory";
import type { CreateChatSessionInput } from "../src/chat/session-factory";
import type { ChatConfig } from "../src/chat/types";
import type { WasmRuntimeStarterOptions } from "../src/wasm/runtime-pool";
import { FakeDatabase } from "./chat-fixtures";
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
    test("per-session WASM pool starts with the session agentConfig snapshot", async () => {
        const starts: WasmRuntimeStarterOptions[] = [];
        const chat = await createChatSessionInternal({
            ...input({
                target: {
                    ...input().target,
                    agent: { settings: { chat: true } },
                    storage: { url: "postgresql://session", httpEndpoint: "https://proxy.example.org/sql" },
                },
                chatConfig: {
                    runtime: { kind: "wasm", env: { BASE: "1" } },
                },
            }),
            // 默认 instanceFactory 走真实 PeriInstance；只替换运行后端启动器。
            wasmRuntimeStarter: async (options) => {
                starts.push(options);
                return new TestTransport(undefined, "session-wasm") as never;
            },
        });

        const ready = await chat.execute(
            ChatUserAction.createNewSession({ requestId: "wasm-1" }),
        );

        expect(ready.sessionId).toBe("session-wasm");
        expect(starts).toHaveLength(1);
        expect(starts[0]!.env).toEqual({ BASE: "1" });
        const snapshot = JSON.parse(starts[0]!.configJson!) as Record<
            string,
            unknown
        >;
        expect(snapshot).toMatchObject({
            cwd: "/workspace",
            settings: { chat: true },
            storage: { url: "postgresql://session", httpEndpoint: "https://proxy.example.org/sql" },
            machineId: "machine-1",
        });
        expect(readChatSessionState(chat)?.sessionId).toBe("session-1");

        await chat.close();
    });

    test("transport runtime assembles without a WASM pool", async () => {
        const chat = await createChatSessionInternal(input());
        // transport 句柄不建立运行池；装配后仍是未绑定状态。
        expect(chat.phase).toBe("empty");
        await chat.close();
        expect(chat.phase).toBe("closed");
    });
});
