import { describe, expect, test } from "bun:test";
import { AcpMcpBridge, resolveAcpMcpBindings } from "../src/agent/acp-mcp-bridge";
import { acpMcpServers } from "../src/agent/peri-instance";
import type { AcpMcpServer, AcpMcpServerInstance, AcpMcpTransport } from "../src/contracts/acp";

const idleServer: AcpMcpServerInstance = { connect: async () => {} };

describe("mcpServers serialization", () => {
    test("acp declarations expose only name/type/serverId, never the server instance", () => {
        const serialized = acpMcpServers({
            "sdk-fetch": { type: "acp", server: idleServer },
            custom: { type: "acp", server: idleServer, serverId: "custom-id" },
        });

        expect(serialized).toEqual([
            { name: "sdk-fetch", type: "acp", serverId: "sdk-fetch" },
            { name: "custom", type: "acp", serverId: "custom-id" },
        ]);
        expect(JSON.stringify(serialized)).not.toContain("connect");
    });

    test("http and stdio declarations keep their existing wire shape", () => {
        const serialized = acpMcpServers({
            remote: { type: "http", url: "https://example.test/mcp", headers: { Authorization: "Bearer x" } },
            local: { type: "stdio", command: "node", args: ["server.js"], env: { TOKEN: "secret" } },
            minimal: { type: "stdio", command: "node" },
        });

        expect(serialized).toEqual([
            {
                name: "remote", type: "http", url: "https://example.test/mcp",
                headers: [{ name: "Authorization", value: "Bearer x" }],
            },
            {
                name: "local", type: "stdio", command: "node", args: ["server.js"],
                env: [{ name: "TOKEN", value: "secret" }],
            },
            { name: "minimal", type: "stdio", command: "node", env: [] },
        ]);
    });

    test("an acp declaration requires a connectable server instance", async () => {
        await expect(resolveAcpMcpBindings({
            broken: { type: "acp", server: {} as AcpMcpServerInstance },
        })).rejects.toThrow('mcpServers["broken"].server must expose connect(transport)');
    });

    test("duplicate explicit serverIds are rejected", async () => {
        await expect(resolveAcpMcpBindings({
            one: { type: "acp", server: idleServer, serverId: "same" },
            two: { type: "acp", server: idleServer, serverId: "same" },
        })).rejects.toThrow("Duplicate MCP serverId: same");
    });

    test("factory instances are owned and closed on resolution failure", async () => {
        let closed = 0;
        const broken = { connect: async () => {}, close: async () => { closed += 1; } };
        await expect(resolveAcpMcpBindings({
            first: { type: "acp", server: async () => broken },
            second: { type: "acp", server: {} as AcpMcpServerInstance },
        })).rejects.toThrow('mcpServers["second"].server must expose connect(transport)');
        expect(closed).toBe(1);
    });
});

type Frame = Record<string, unknown>;

/** 模拟官方 `Server`：connect 安装 transport.onmessage，响应经 transport.send 发出。 */
class FakeMcpServer implements AcpMcpServerInstance {
    connects = 0;
    closed = false;
    transport?: AcpMcpTransport;
    readonly received: Frame[] = [];
    onRequest?: (frame: Frame) => void;

    async connect(transport: AcpMcpTransport): Promise<void> {
        this.connects += 1;
        this.transport = transport;
        transport.onmessage = (message) => this.receive(message as Frame);
    }

    receive(frame: Frame): void {
        this.received.push(frame);
        this.onRequest?.(frame);
    }

    reply(id: unknown, result: unknown): Promise<void> {
        return this.transport!.send({ jsonrpc: "2.0", id, result });
    }

    fail(id: unknown, error: { code: number; message: string; data?: unknown }): Promise<void> {
        return this.transport!.send({ jsonrpc: "2.0", id, error });
    }
}

const acpDeclaration = (server: AcpMcpServerInstance) => ([
    { serverId: "sdk-fetch", instance: server, owned: false },
]);

async function waitFor(predicate: () => boolean, timeoutMs = 1000): Promise<void> {
    const startedAt = Date.now();
    while (!predicate()) {
        if (Date.now() - startedAt > timeoutMs) throw new Error("condition was not reached");
        await new Promise((resolve) => setTimeout(resolve, 1));
    }
}

describe("mcp/message bridge", () => {
    test("maps an official-style result response", async () => {
        const server = new FakeMcpServer();
        server.onRequest = (frame) => {
            void server.reply(frame.id, { tools: [{ name: "fetch" }] });
        };
        const bridge = new AcpMcpBridge(acpDeclaration(server));

        const response = await bridge.handle({
            serverId: "sdk-fetch", requestId: "peri-1", method: "tools/list", params: {},
        });

        expect(response).toEqual({ result: { tools: [{ name: "fetch" }] } });
        expect(server.received[0]).toEqual({
            jsonrpc: "2.0", id: "peri-1", method: "tools/list", params: {},
        });
    });

    test("maps JSON-RPC errors, including data, unchanged", async () => {
        const server = new FakeMcpServer();
        server.onRequest = (frame) => {
            void server.fail(frame.id, { code: -32602, message: "bad arguments", data: { field: "url" } });
        };
        const bridge = new AcpMcpBridge(acpDeclaration(server));

        const response = await bridge.handle({
            serverId: "sdk-fetch", requestId: "peri-2", method: "tools/call", params: {},
        });

        expect(response).toEqual({
            error: { code: -32602, message: "bad arguments", data: { field: "url" } },
        });
    });

    test("propagates MethodNotFound (-32601) instead of pretending success", async () => {
        const server = new FakeMcpServer();
        server.onRequest = (frame) => {
            void server.fail(frame.id, { code: -32601, message: "Method not found" });
        };
        const bridge = new AcpMcpBridge(acpDeclaration(server));

        const response = await bridge.handle({
            serverId: "sdk-fetch", requestId: "peri-3", method: "server/discover", params: {},
        });

        expect(response).toEqual({ error: { code: -32601, message: "Method not found" } });
    });

    test("delivers notifications without an id and answers immediately", async () => {
        const server = new FakeMcpServer();
        const bridge = new AcpMcpBridge(acpDeclaration(server));

        const response = await bridge.handle({
            serverId: "sdk-fetch", requestId: "peri-4", method: "notifications/initialized",
        });

        expect(response).toEqual({ result: {} });
        expect(server.received).toEqual([{ jsonrpc: "2.0", method: "notifications/initialized" }]);
        expect(server.connects).toBe(1);
    });

    test("rejects an unknown serverId with an explicit error", async () => {
        const bridge = new AcpMcpBridge(acpDeclaration(new FakeMcpServer()));

        const response = await bridge.handle({
            serverId: "other", requestId: "peri-5", method: "tools/list",
        });

        expect(response).toEqual({ error: { code: -32001, message: "Unknown MCP serverId: other" } });
    });

    test("connects the server instance once across calls", async () => {
        const server = new FakeMcpServer();
        server.onRequest = (frame) => { void server.reply(frame.id, {}); };
        const bridge = new AcpMcpBridge(acpDeclaration(server));

        await bridge.connect();
        await bridge.connect();
        await bridge.handle({ serverId: "sdk-fetch", requestId: "a", method: "ping" });
        await bridge.handle({ serverId: "sdk-fetch", requestId: "b", method: "ping" });

        expect(server.connects).toBe(1);
    });

    test("ignores unsolicited server notifications while a request is pending", async () => {
        const server = new FakeMcpServer();
        server.onRequest = (frame) => {
            void server.transport!.send({ jsonrpc: "2.0", method: "notifications/message", params: { level: "info" } });
            void server.reply(frame.id, { ok: true });
        };
        const bridge = new AcpMcpBridge(acpDeclaration(server));

        const response = await bridge.handle({
            serverId: "sdk-fetch", requestId: "peri-6", method: "tools/call", params: {},
        });

        expect(response).toEqual({ result: { ok: true } });
    });

    test("abortPending rejects in-flight calls without closing the external server", async () => {
        const server = new FakeMcpServer();
        const bridge = new AcpMcpBridge(acpDeclaration(server));

        const pending = bridge.handle({
            serverId: "sdk-fetch", requestId: "peri-7", method: "tools/call", params: {},
        });
        await waitFor(() => server.received.length === 1);
        bridge.abortPending();

        await expect(pending).rejects.toThrow("ACP session ended before the MCP request completed");
        expect(server.closed).toBe(false);
    });

    test("a second activation cannot reuse an already connected server instance", async () => {
        const server = new FakeMcpServer();
        const first = new AcpMcpBridge(acpDeclaration(server));
        const second = new AcpMcpBridge(acpDeclaration(server));

        await first.connect();
        await expect(second.connect()).rejects.toThrow("direct acp instances are single-use");
    });
});
