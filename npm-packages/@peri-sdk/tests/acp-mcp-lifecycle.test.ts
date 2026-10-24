import { expect, test } from "bun:test";
import {
    PeriInstance,
    PeriInstanceBindError,
    PeriInstanceCloseIncompleteError,
} from "../src/agent/peri-instance";
import type { AcpMcpServer, AcpMcpServerInstance, AcpMcpTransport } from "../src/agent/types";
import type { ChatFact } from "../src/chat/facts";
import type { JsonRpcNotification, ReverseRequestHandler, Transport } from "../src/transport/types";

class FakeServer implements AcpMcpServerInstance {
    connects = 0;
    closes = 0;
    connectFailures = 0;
    closeFailures = 0;
    transport?: AcpMcpTransport;

    async connect(transport: AcpMcpTransport): Promise<void> {
        this.connects++;
        if (this.connectFailures > 0) {
            this.connectFailures--;
            throw new Error("connect failed");
        }
        this.transport = transport;
        transport.onmessage = () => {};
    }

    async close(): Promise<void> {
        this.closes++;
        if (this.closeFailures > 0) {
            this.closeFailures--;
            throw new Error("close failed");
        }
    }
}

class FakeTransport implements Transport {
    readonly calls: Array<{ method: string; params?: unknown }> = [];
    readonly listeners = new Set<(event: JsonRpcNotification) => void>();
    reverseHandler?: ReverseRequestHandler;
    closeCount = 0;

    async request<Response>(method: string, _params?: unknown): Promise<Response> {
        this.calls.push({ method, params: _params });
        if (method === "initialize") return { protocolVersion: 1 } as Response;
        if (method === "session/new") return { sessionId: "session-new" } as Response;
        if (method === "session/load") return { sessionId: "session-loaded" } as Response;
        if (method === "session/input/snapshot") return { generation: "generation-1" } as Response;
        if (method === "session/rename" || method === "session/close") return {} as Response;
        throw new Error(`unexpected method: ${method}`);
    }

    async sendRequest<Response>(method: string, params?: unknown): Promise<{ response: Promise<Response> }> {
        return { response: this.request<Response>(method, params) };
    }

    async notify(method: string, params?: unknown): Promise<void> {
        this.calls.push({ method, params });
    }

    async *events(): AsyncIterable<JsonRpcNotification> {}

    subscribe(listener: (event: JsonRpcNotification) => void): () => void {
        this.listeners.add(listener);
        return () => { this.listeners.delete(listener); };
    }

    setRequestHandler(handler: ReverseRequestHandler): void {
        this.reverseHandler = handler;
    }

    async close(): Promise<void> {
        this.closeCount++;
    }
}

class FakeHost {
    closed = 0;
    constructor(readonly transport: FakeTransport) {}
    async close(): Promise<{ confirmed: boolean }> {
        this.closed++;
        await this.transport.close();
        return { confirmed: true };
    }
}

function setup(defaultMcpServers: Record<string, AcpMcpServer>) {
    const transport = new FakeTransport();
    const host = new FakeHost(transport);
    const instance = new PeriInstance({
        acquireHost: async () => host,
        resolveSessionPath: async (sessionId) => `/persisted/${sessionId}`,
        defaultMcpServers,
        payloadNonce: "mcp-test",
        close: { drainTimeoutMs: 50, bindingSettleTimeoutMs: 50, activationCloseTimeoutMs: 50 },
    });
    const facts: ChatFact[] = [];
    instance.attach((fact) => facts.push(fact));
    return { transport, host, instance, facts };
}

test("a factory resolves a fresh owned instance per activation and releases it on close", async () => {
    const created: FakeServer[] = [];
    const servers: Record<string, AcpMcpServer> = {
        shared: {
            type: "acp",
            server: () => {
                const server = new FakeServer();
                created.push(server);
                return server;
            },
        },
    };
    const first = setup(servers);
    await first.instance.bindNew({ path: "/workspace" });
    expect(created).toHaveLength(1);
    expect(created[0]!.connects).toBe(1);
    expect(first.transport.calls.find((call) => call.method === "session/new")?.params)
        .toMatchObject({ mcpServers: [{ name: "shared", type: "acp", serverId: "shared" }] });
    expect(JSON.stringify(first.transport.calls.find((call) => call.method === "session/new")?.params))
        .not.toContain("connect");

    await first.instance.close();
    expect(created[0]!.closes).toBe(1);
    expect(first.host.closed).toBe(1);

    // 第二次激活仍在同一个声明上成功：工厂不会返回已消费的实例。
    const second = setup(servers);
    await second.instance.bindNew({ path: "/workspace" });
    expect(created).toHaveLength(2);
    expect(created[1]!.connects).toBe(1);
    await second.instance.close();
});

test("a borrowed direct instance is single-use and its binding is retained", async () => {
    const server = new FakeServer();
    const servers: Record<string, AcpMcpServer> = {
        borrowed: { type: "acp", server },
    };
    const first = setup(servers);
    await first.instance.bindNew({ path: "/workspace" });
    await first.instance.close();
    // 借用实例由调用方持有，bridge close 不关闭它。
    expect(server.closes).toBe(0);

    const second = setup(servers);
    const failure = await second.instance.bindNew({ path: "/workspace" }).catch((error) => error);
    expect(failure).toBeInstanceOf(PeriInstanceBindError);
    expect(failure).toMatchObject({ stage: "mcp-connect" });
    expect((failure.cause as Error).message).toContain("single-use");
    expect(second.host.closed).toBe(1);
});

test("a factory result without close() is rejected at mcp-connect", async () => {
    const servers: Record<string, AcpMcpServer> = {
        broken: { type: "acp", server: () => ({ connect: async () => {} }) },
    };
    const { instance, host } = setup(servers);
    const failure = await instance.bindNew({ path: "/workspace" }).catch((error) => error);
    expect(failure).toBeInstanceOf(PeriInstanceBindError);
    expect(failure).toMatchObject({ stage: "mcp-connect", createdSessionId: null });
    expect(failure.cause).toBeInstanceOf(TypeError);
    expect(String((failure.cause as Error).message)).toContain("close()");
    expect(host.closed).toBe(1);
});

test("a mid-resolution failure closes the owned instances created before it", async () => {
    const first = new FakeServer();
    const servers: Record<string, AcpMcpServer> = {
        one: { type: "acp", server: () => first },
        two: { type: "acp", server: () => { throw new Error("factory blew up"); } },
    };
    const { instance, host } = setup(servers);
    const failure = await instance.bindNew({ path: "/workspace" }).catch((error) => error);
    expect(failure).toMatchObject({ name: "PeriInstanceBindError", stage: "mcp-connect" });
    expect(first.closes).toBe(1);
    expect(host.closed).toBe(1);
    expect(instance.needsReload).toBe(false);
});

test("an owned connect failure closes and unbinds the instance for a later activation", async () => {
    const server = new FakeServer();
    server.connectFailures = 1;
    const servers: Record<string, AcpMcpServer> = {
        owned: { type: "acp", server: () => server },
    };
    const first = setup(servers);
    const failure = await first.instance.bindNew({ path: "/workspace" }).catch((error) => error);
    expect(failure).toMatchObject({ stage: "mcp-connect" });
    expect(server.closes).toBe(1);

    const second = setup(servers);
    await second.instance.bindNew({ path: "/workspace" });
    expect(server.connects).toBe(2);
    await second.instance.close();
    expect(server.closes).toBe(2);
});

test("a borrowed connect failure consumes the instance instead of retrying it", async () => {
    const server = new FakeServer();
    server.connectFailures = 1;
    const servers: Record<string, AcpMcpServer> = {
        borrowed: { type: "acp", server },
    };
    const first = setup(servers);
    const failure = await first.instance.bindNew({ path: "/workspace" }).catch((error) => error);
    expect(failure).toMatchObject({ stage: "mcp-connect" });
    expect(server.connects).toBe(1);

    const second = setup(servers);
    const retry = await second.instance.bindNew({ path: "/workspace" }).catch((error) => error);
    expect(retry).toMatchObject({ name: "PeriInstanceBindError", stage: "mcp-connect" });
    expect((retry.cause as Error).message).toContain("single-use");
    expect(server.connects).toBe(1);
});

test("an owned close failure keeps the bridge and the second close completes", async () => {
    const server = new FakeServer();
    server.closeFailures = 1;
    const servers: Record<string, AcpMcpServer> = {
        owned: { type: "acp", server: () => server },
    };
    const { host, instance, facts } = setup(servers);
    await instance.bindNew({ path: "/workspace" });

    const first = await instance.close().catch((error) => error);
    expect(first).toBeInstanceOf(PeriInstanceCloseIncompleteError);
    expect(server.closes).toBe(1);
    expect(host.closed).toBe(0);
    expect(instance.needsReload).toBe(true);
    expect(facts.some((fact) => fact.kind === "closed")).toBe(false);

    await instance.close();
    expect(server.closes).toBe(2);
    expect(host.closed).toBe(1);
    expect(instance.needsReload).toBe(false);
    expect(facts.filter((fact) => fact.kind === "closed")).toHaveLength(1);
});
