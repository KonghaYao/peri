import { describe, expect, test } from "bun:test";
import { ChatUserAction } from "../src/chat/action";
import { ChatManager } from "../src/chat/manager";
import { observeBackend } from "../src/entries/observe";
import { SandboxManager } from "../src/sandbox/manager";
import type {
    SandboxAdapter,
    SandboxAdapterInstance,
    SandboxAdapterStartOptions,
} from "../src/sandbox/types";
import { FakeDatabase, FakePort } from "./chat-fixtures";
import { OwnershipProbe } from "./ownership-probe";

const TEST_WORKSPACE = { id: "ws-observe", runtimePath: "/workspace" };
const TOKEN = "token-must-never-appear-123";
const ENV_SECRET = "env-secret-must-never-appear";
const ARG_SECRET = "arg-secret-must-never-appear";
const SANDBOX_QUERY_SECRET = "sandbox-query-must-never-appear";
const STOP_ERROR_SECRET = "stop-error-secret-must-never-appear";

const SANDBOX_A = `sbx_${"a".repeat(32)}`;
const SANDBOX_B = `sbx_${"b".repeat(32)}`;

function transportSetup(configure?: (port: FakePort) => void): {
    manager: ChatManager;
    ports: FakePort[];
} {
    const ports: FakePort[] = [];
    const manager = new ChatManager({
        store: { storage: {}, machineId: "test-machine" },
        database: new FakeDatabase(),
        kv: new OwnershipProbe(),
        chatConfig: {
            runtime: {
                kind: "transport",
                factory: () => {
                    throw new Error("ACP transport must not start in observe tests");
                },
            },
            session: {},
        },
        instanceFactory: () => {
            const port = new FakePort();
            configure?.(port);
            ports.push(port);
            return port;
        },
    });
    return { manager, ports };
}

class FakeSandboxInstance implements SandboxAdapterInstance {
    readonly workspace: {
        id: string;
        hostPath: string;
        runtimePath: string;
    };
    readonly mcpServer: {
        type: "http";
        url: string;
        headers: Record<string, string>;
    };
    closeError?: unknown;

    constructor(
        readonly sandboxId: string,
        port: number,
    ) {
        this.workspace = {
            id: sandboxId,
            hostPath: `/tmp/sandboxes/${sandboxId}`,
            runtimePath: `/tmp/sandboxes/${sandboxId}`,
        };
        this.mcpServer = {
            type: "http",
            url: `http://127.0.0.1:${port}/mcp?token=${SANDBOX_QUERY_SECRET}`,
            headers: { Authorization: `Bearer ${TOKEN}` },
        };
    }

    async close(): Promise<void> {
        if (this.closeError) throw this.closeError;
    }
}

class FakeSandboxAdapter implements SandboxAdapter {
    readonly starts: string[] = [];
    readonly instances = new Map<string, FakeSandboxInstance>();

    async start(
        options: SandboxAdapterStartOptions,
    ): Promise<SandboxAdapterInstance> {
        this.starts.push(options.sandboxId);
        const instance = new FakeSandboxInstance(
            options.sandboxId,
            4200 + this.starts.length,
        );
        this.instances.set(options.sandboxId, instance);
        return instance;
    }

    async deleteData(): Promise<void> {}
}

describe("observeBackend chat projection", () => {
    test("transport runtime reports a bound chat with sanitized MCP config", async () => {
        const { manager, ports } = transportSetup();
        const chat = manager.createChat({
            workspace: TEST_WORKSPACE,
            agent: {},
            session: {
                mcpServers: {
                    remote: {
                        type: "http",
                        url: `http://user:${TOKEN}@127.0.0.1:4100/mcp?token=${TOKEN}#fragment`,
                        headers: {
                            Authorization: `Bearer ${TOKEN}`,
                            "X-Api-Key": TOKEN,
                        },
                    },
                    "local-tool": {
                        type: "stdio",
                        command: "node",
                        args: ["--token", ARG_SECRET],
                        env: { SECRET: ENV_SECRET },
                    },
                },
            },
        });

        const empty = observeBackend({ chats: manager });
        expect(empty.agents).toHaveLength(1);
        expect(empty.agents[0]!.chat.phase).toBe("empty");
        expect(empty.agents[0]!.chat.instance).toBeNull();
        expect(empty.agents[0]!.chat.mcp).toEqual({
            source: "unknown",
            servers: [],
        });
        expect(ports).toHaveLength(0);

        await chat.execute(
            ChatUserAction.createNewSession({ requestId: "observe-create" }),
        );
        const snapshot = observeBackend({ chats: manager });
        const agent = snapshot.agents[0]!;

        expect(agent.agentId).toBe(chat.id);
        expect(agent.chat.chatId).toBe(chat.id);
        expect(agent.chat.sessionId).toBe("session-new");
        expect(agent.chat.bindingRevision).toBe(1);
        expect(agent.chat.phase).toBe("bound");
        expect(agent.chat.phaseLimited).toBe(true);
        expect(agent.chat.workspace).toEqual({
            id: TEST_WORKSPACE.id,
            runtimePath: TEST_WORKSPACE.runtimePath,
        });
        expect(agent.chat.instance).toEqual({
            state: "live",
            running: false,
            pendingInputCount: 0,
            pendingInteractionCount: 0,
        });
        expect(agent.chat.mcp.source).toBe("session-init");
        expect(agent.chat.mcp.servers).toEqual([
            {
                name: "local-tool",
                type: "stdio",
                source: "config",
                health: "unknown",
            },
            {
                name: "remote",
                type: "http",
                source: "config",
                endpoint: "http://127.0.0.1:4100/mcp",
                headerNames: ["Authorization", "X-Api-Key"],
                health: "unknown",
            },
        ]);
        expect(agent.sandbox).toBeNull();
        expect(snapshot.orphanSandboxes).toEqual([]);
        expect(snapshot.totals).toEqual({
            agents: 1,
            chats: 1,
            activeInstances: 1,
            mcpServers: 2,
            sandboxMcpServers: 0,
            sandboxes: 0,
        });
        expect(Number.isNaN(Date.parse(snapshot.observedAt))).toBe(false);

        const serialized = JSON.stringify(snapshot);
        expect(serialized).not.toContain(TOKEN);
        expect(serialized).not.toContain(ENV_SECRET);
        expect(serialized).not.toContain(ARG_SECRET);
        expect(serialized).not.toContain("--token");
        expect(serialized).not.toContain("SECRET");
    });

    test("bindExisting marks MCP config as unknown instead of inventing servers", async () => {
        const { manager } = transportSetup();
        const chat = manager.createChat({
            workspace: TEST_WORKSPACE,
            agent: {},
            session: {
                mcpServers: {
                    remote: { type: "http", url: "http://127.0.0.1:4100/mcp" },
                },
            },
        });

        await chat.execute(
            ChatUserAction.createNewSession({ requestId: "observe-new" }),
        );
        expect(
            observeBackend({ chats: manager }).agents[0]!.chat.mcp.source,
        ).toBe("session-init");

        await chat.execute(
            ChatUserAction.changeSessionTo("session-existing", {
                requestId: "observe-load",
            }),
        );
        const agent = observeBackend({ chats: manager }).agents[0]!;
        expect(agent.chat.sessionId).toBe("session-existing");
        expect(agent.chat.bindingRevision).toBe(2);
        expect(agent.chat.phase).toBe("bound");
        expect(agent.chat.mcp).toEqual({ source: "unknown", servers: [] });
    });

    test("empty chat is projected without starting an instance; closed is closing", async () => {
        const { manager, ports } = transportSetup();
        const chat = manager.createChat({
            workspace: TEST_WORKSPACE,
            agent: {},
        });
        expect(observeBackend({ chats: manager }).agents[0]!.chat.phase).toBe(
            "empty",
        );
        expect(ports).toHaveLength(0);

        await chat.execute(
            ChatUserAction.createNewSession({ requestId: "observe-close" }),
        );
        await chat.close();

        const closed = chat.observe();
        expect(closed.phase).toBe("closing");
        expect(closed.sessionId).toBe("session-new");
        expect(closed.instance).toBeNull();
        expect(manager.observe()).toEqual([]);
    });
});

describe("observeBackend sandbox composition", () => {
    test("matches sandbox by MCP endpoint, keeps orphans and totals deterministic", async () => {
        const adapter = new FakeSandboxAdapter();
        const sandboxes = new SandboxManager({ adapter });
        // 创建顺序与 sandboxId 排序相反，验证快照稳定排序。
        const sandboxB = await sandboxes.start({ sandboxId: SANDBOX_B });
        const sandboxA = await sandboxes.start({ sandboxId: SANDBOX_A });
        const server = await sandboxA.mcpServer();

        const { manager } = transportSetup();
        const chat = manager.createChat({
            workspace: TEST_WORKSPACE,
            agent: {},
            session: {
                mcpServers: {
                    [server.name]: {
                        type: "http",
                        url: server.config.url,
                        headers: server.config.headers,
                    },
                },
            },
        });
        await chat.execute(
            ChatUserAction.createNewSession({ requestId: "observe-sandbox" }),
        );

        const handleStatesBefore = sandboxes.list().map((handle) => ({
            sandboxId: handle.sandboxId,
            state: handle.state,
        }));
        const snapshot = observeBackend({ chats: manager, sandboxes });

        expect(adapter.starts).toEqual([SANDBOX_B, SANDBOX_A]);
        expect(sandboxes.snapshot().map((item) => item.sandboxId)).toEqual([
            SANDBOX_A,
            SANDBOX_B,
        ]);
        expect(sandboxes.list().map((handle) => ({
            sandboxId: handle.sandboxId,
            state: handle.state,
        }))).toEqual(handleStatesBefore);

        const agent = snapshot.agents[0]!;
        expect(agent.sandbox?.sandboxId).toBe(SANDBOX_A);
        expect(agent.sandbox).toMatchObject({
            sandboxId: SANDBOX_A,
            state: "ready",
            serverName: "workspace",
            mcpEndpoint: "http://127.0.0.1:4202/mcp",
            workspace: {
                id: SANDBOX_A,
                hostPath: `/tmp/sandboxes/${SANDBOX_A}`,
                runtimePath: `/tmp/sandboxes/${SANDBOX_A}`,
            },
            lastError: null,
        });
        expect(agent.chat.mcp.servers[0]!.source).toBe("sandbox");
        expect(agent.chat.mcp.servers[0]!.endpoint).toBe(
            "http://127.0.0.1:4202/mcp",
        );
        expect(snapshot.orphanSandboxes.map((item) => item.sandboxId)).toEqual([
            SANDBOX_B,
        ]);
        expect(snapshot.orphanSandboxes[0]!.mcpEndpoint).toBe(
            "http://127.0.0.1:4201/mcp",
        );
        expect(snapshot.totals).toEqual({
            agents: 1,
            chats: 1,
            activeInstances: 1,
            mcpServers: 1,
            sandboxMcpServers: 1,
            sandboxes: 2,
        });

        const serialized = JSON.stringify(snapshot);
        expect(serialized).not.toContain(TOKEN);
        expect(serialized).not.toContain(SANDBOX_QUERY_SECRET);

        const stopError = new Error(
            `stop failed at http://127.0.0.1:9/x?token=${STOP_ERROR_SECRET}`,
        );
        adapter.instances.get(SANDBOX_B)!.closeError = stopError;
        await expect(sandboxB.stop()).rejects.toThrow();
        const failed = observeBackend({ chats: manager, sandboxes })
            .orphanSandboxes.find((item) => item.sandboxId === SANDBOX_B)!;
        expect(failed.state).toBe("failed");
        expect(failed.mcpEndpoint).toBeNull();
        expect(failed.lastError?.code).toBe("stop_incomplete");
        expect(failed.lastError?.message).not.toContain(STOP_ERROR_SECRET);
        // URL 收敛为 origin + pathname，query 不进快照。
        expect(failed.lastError?.message).toContain("http://127.0.0.1:9/x");
    });

    test("shares one sandbox across every chat that references its endpoint", async () => {
        const adapter = new FakeSandboxAdapter();
        const sandboxes = new SandboxManager({ adapter });
        const handle = await sandboxes.start({ sandboxId: SANDBOX_A });
        const server = await handle.mcpServer();

        let sessionIndex = 0;
        const { manager } = transportSetup((port) => {
            sessionIndex += 1;
            port.bindNewSessionId = `session-shared-${sessionIndex}`;
        });
        const sessionConfig = () => ({
            mcpServers: {
                [server.name]: {
                    type: "http" as const,
                    url: server.config.url,
                    headers: { ...server.config.headers },
                },
            },
        });
        const first = manager.createChat({
            workspace: TEST_WORKSPACE,
            agent: {},
            session: sessionConfig(),
        });
        const second = manager.createChat({
            workspace: TEST_WORKSPACE,
            agent: {},
            session: sessionConfig(),
        });
        await first.execute(
            ChatUserAction.createNewSession({ requestId: "observe-shared-1" }),
        );
        await second.execute(
            ChatUserAction.createNewSession({ requestId: "observe-shared-2" }),
        );

        const snapshot = observeBackend({ chats: manager, sandboxes });
        expect(snapshot.agents).toHaveLength(2);
        expect(snapshot.agents.map((agent) => agent.agentId)).toEqual(
            [first.id, second.id].sort(),
        );
        for (const agent of snapshot.agents) {
            expect(agent.sandbox?.sandboxId).toBe(SANDBOX_A);
            expect(agent.chat.mcp.servers[0]!.source).toBe("sandbox");
        }
        expect(snapshot.agents[0]!.sandbox).toEqual(
            snapshot.agents[1]!.sandbox,
        );
        expect(snapshot.orphanSandboxes).toEqual([]);
        expect(snapshot.totals).toEqual({
            agents: 2,
            chats: 2,
            activeInstances: 2,
            mcpServers: 2,
            sandboxMcpServers: 2,
            sandboxes: 1,
        });
    });
});

describe("observeBackend stability and read-only behavior", () => {
    test("sorts agents by chatId regardless of source order and never mutates chats", async () => {
        const { manager, ports } = transportSetup();
        const first = manager.createChat({
            workspace: TEST_WORKSPACE,
            agent: {},
        });
        const second = manager.createChat({
            workspace: TEST_WORKSPACE,
            agent: {},
        });
        await second.execute(
            ChatUserAction.createNewSession({ requestId: "observe-sort-2" }),
        );

        const callsBefore = ports.map((port) => [...port.calls]);
        const snapshot = observeBackend({ chats: manager });
        const sortedIds = [...snapshot.agents.map((agent) => agent.agentId)]
            .sort();
        expect(snapshot.agents.map((agent) => agent.agentId)).toEqual(sortedIds);
        expect(snapshot.agents).toHaveLength(2);
        expect(snapshot.totals).toEqual({
            agents: 2,
            chats: 2,
            activeInstances: 1,
            mcpServers: 0,
            sandboxMcpServers: 0,
            sandboxes: 0,
        });

        const reversed = observeBackend({
            chats: {
                observe: () => [...manager.observe()].reverse(),
            },
        });
        expect(reversed.agents.map((agent) => agent.agentId)).toEqual(
            sortedIds,
        );

        const again = observeBackend({ chats: manager });
        expect(again.agents).toEqual(snapshot.agents);
        expect(again.totals).toEqual(snapshot.totals);
        expect(ports).toHaveLength(1);
        expect(ports.map((port) => [...port.calls])).toEqual(callsBefore);
        expect(first.phase).toBe("empty");
    });

    test("rejects inputs without sync observe/snapshot methods", () => {
        expect(() =>
            observeBackend({ chats: {} as never }),
        ).toThrow(TypeError);
        expect(() =>
            observeBackend({
                chats: { observe: () => [] },
                sandboxes: {} as never,
            }),
        ).toThrow(TypeError);
    });
});
