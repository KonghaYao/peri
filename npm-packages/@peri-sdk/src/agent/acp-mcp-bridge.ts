import type { AcpMcpServer, AcpMcpServerInstance, AcpMcpTransport } from "./types";

/** ACP `mcp/message` 的响应体：`{result}` 或包装后的 JSON-RPC 错误。 */
type AcpMcpResponse =
    | { result: unknown }
    | { error: { code: number; message: string; data?: unknown } };

type JsonRpcId = string | number;

type PendingCall = {
    resolve: (message: Record<string, unknown>) => void;
    reject: (error: Error) => void;
};

/** 每个 acp 声明一条桥接 transport；server 实例的生命周期仍归调用方。 */
class AcpMcpBridgeTransport implements AcpMcpTransport {
    onclose?: () => void;
    onerror?: (error: Error) => void;
    onmessage?: (message: unknown, extra?: unknown) => void;
    sessionId?: string;

    private readonly pending = new Map<string, PendingCall>();
    private closed = false;

    constructor(private readonly serverId: string) {}

    async start(): Promise<void> {
        // 反向 ACP 通道已经可用；这里只完成官方 Protocol 的启动握手。
    }

    async send(message: unknown): Promise<void> {
        if (this.closed) throw new Error(`MCP transport for "${this.serverId}" is closed`);
        const frame = asRecord(message);
        if (!frame) {
            this.reportError(new Error(`MCP server "${this.serverId}" sent a non-object message`));
            return;
        }
        const method = typeof frame.method === "string" ? frame.method : undefined;
        const id = rpcId(frame.id);
        if (method === undefined) {
            // 响应：只结算本桥接发出的调用；迟到的响应不改变任何状态。
            if (id === undefined) return;
            const key = idKey(id);
            const pending = this.pending.get(key);
            if (!pending) {
                console.warn(`[peri-sdk] Ignoring unsolicited MCP response from "${this.serverId}"`);
                return;
            }
            this.pending.delete(key);
            pending.resolve(frame);
            return;
        }
        if (id === undefined) {
            // 主动通知：官方 server 不等待响应，忽略并记录，不影响在途请求。
            console.warn(`[peri-sdk] Ignoring unsolicited MCP notification from "${this.serverId}": ${method}`);
            return;
        }
        // server 发起的请求（sampling/roots 等）：SDK 未声明这些客户端能力，明确拒绝。
        this.deliver({
            jsonrpc: "2.0",
            id,
            error: { code: -32601, message: `Method not found: ${method}` },
        });
    }

    async close(): Promise<void> {
        this.closed = true;
        this.abortPending(new Error(`MCP server "${this.serverId}" closed the transport`));
    }

    /** 把一帧投递给官方 server 的 `onmessage`；未 connect 时明确失败。 */
    deliver(message: unknown): void {
        if (!this.onmessage) throw new Error(`MCP server "${this.serverId}" is not connected`);
        this.onmessage(message);
    }

    addPending(key: string, call: PendingCall): void {
        this.pending.set(key, call);
    }

    deletePending(key: string): void {
        this.pending.delete(key);
    }

    abortPending(error: Error): void {
        const pending = [...this.pending.values()];
        this.pending.clear();
        for (const call of pending) call.reject(error);
    }

    private reportError(error: Error): void {
        this.onerror?.(error);
    }
}

type BridgeEntry = {
    serverId: string;
    instance: AcpMcpServerInstance;
    transport: AcpMcpBridgeTransport;
    connecting?: Promise<void>;
};

/**
 * 同一 server 实例只能绑定一条官方 transport：官方 `Protocol.connect` 会拒绝重复连接，
 * 重连语义不在 SDK 内模拟。实例只被弱引用，调用方仍持有其生命周期。
 */
const boundInstances = new WeakMap<object, AcpMcpBridgeTransport>();

/**
 * 反向 ACP `mcp/message` 桥接：把 Agent 声明的官方 MCP server 实例投影到 ACP 客户端请求上。
 *
 * 只处理 `{serverId, requestId, method, params}`；身份校验失败返回明确错误，不静默成功。
 * 关闭 Session 只 reject 未决请求，不关闭外部 server 实例。
 */
export class AcpMcpBridge {
    private readonly entries = new Map<string, BridgeEntry>();

    constructor(servers: Record<string, AcpMcpServer> | undefined) {
        for (const [name, server] of Object.entries(servers ?? {})) {
            if (server.type !== "acp") continue;
            if (typeof server.server?.connect !== "function")
                throw new TypeError(`mcpServers["${name}"].server must expose connect(transport)`);
            const serverId = server.serverId ?? name;
            if (this.entries.has(serverId))
                throw new Error(`Duplicate MCP serverId: ${serverId}`);
            this.entries.set(serverId, {
                serverId,
                instance: server.server,
                transport: new AcpMcpBridgeTransport(serverId),
            });
        }
    }

    /** 连接全部声明；重复调用只等待同一个连接结果。 */
    async connect(): Promise<void> {
        await Promise.all([...this.entries.values()].map((entry) => this.connectEntry(entry)));
    }

    /**
     * 处理一条 ACP 反向 `mcp/message`，返回 ACP 响应体（`{result}` 或 `{error}`）。
     *
     * `notifications/*` 投递后立即返回空 result；普通请求等待官方 server 的响应。
     */
    async handle(params: unknown): Promise<AcpMcpResponse> {
        const message = asRecord(params);
        const serverId = typeof message?.serverId === "string" ? message.serverId : undefined;
        const entry = serverId === undefined ? undefined : this.entries.get(serverId);
        if (!entry) {
            return {
                error: {
                    code: -32001,
                    message: serverId === undefined
                        ? "mcp/message requires a serverId"
                        : `Unknown MCP serverId: ${serverId}`,
                },
            };
        }
        const method = typeof message?.method === "string" ? message.method : undefined;
        if (!method) return { error: { code: -32600, message: "mcp/message requires a method" } };
        const id = rpcId(message?.requestId);
        if (id === undefined) return { error: { code: -32600, message: "mcp/message requires a requestId" } };

        try {
            await this.connectEntry(entry);
        } catch (error) {
            return {
                error: {
                    code: -32603,
                    message: `MCP server "${entry.serverId}" failed to connect: ${errorMessage(error)}`,
                },
            };
        }

        const innerParams = message?.params;
        if (method.startsWith("notifications/")) {
            try {
                entry.transport.deliver({
                    jsonrpc: "2.0",
                    method,
                    ...(innerParams === undefined ? {} : { params: innerParams }),
                });
            } catch (error) {
                return { error: { code: -32603, message: errorMessage(error) } };
            }
            return { result: {} };
        }

        const key = idKey(id);
        const response = await new Promise<Record<string, unknown>>((resolve, reject) => {
            entry.transport.addPending(key, { resolve, reject });
            try {
                entry.transport.deliver({
                    jsonrpc: "2.0",
                    id,
                    method,
                    ...(innerParams === undefined ? {} : { params: innerParams }),
                });
            } catch (error) {
                entry.transport.deletePending(key);
                reject(error instanceof Error ? error : new Error(String(error)));
            }
        });
        return mapServerResponse(response);
    }

    /** Session 关闭时结算未决请求；外部 server 实例保持调用方持有。 */
    abortPending(reason = "ACP session ended before the MCP request completed"): void {
        const error = new Error(reason);
        for (const entry of this.entries.values()) entry.transport.abortPending(error);
    }

    private connectEntry(entry: BridgeEntry): Promise<void> {
        if (entry.connecting) return entry.connecting;
        const bound = boundInstances.get(entry.instance);
        if (bound && bound !== entry.transport) {
            entry.connecting = Promise.reject(new Error(
                `MCP server instance for serverId "${entry.serverId}" is already connected; use a separate server instance per Agent`,
            ));
            return entry.connecting;
        }
        boundInstances.set(entry.instance, entry.transport);
        entry.connecting = Promise.resolve()
            .then(() => entry.instance.connect(entry.transport))
            .catch((error) => {
                throw new Error(`MCP server "${entry.serverId}" connect failed: ${errorMessage(error)}`, { cause: error });
            });
        return entry.connecting;
    }
}

function mapServerResponse(message: Record<string, unknown>): AcpMcpResponse {
    if ("error" in message) {
        const error = asRecord(message.error);
        return {
            error: {
                code: typeof error?.code === "number" ? error.code : -32603,
                message: typeof error?.message === "string" ? error.message : "MCP server request failed",
                ...(error && error.data !== undefined ? { data: error.data } : {}),
            },
        };
    }
    if ("result" in message) return { result: message.result };
    return { error: { code: -32603, message: "MCP server returned neither result nor error" } };
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
    return typeof value === "object" && value !== null && !Array.isArray(value)
        ? value as Record<string, unknown>
        : undefined;
}

function rpcId(value: unknown): JsonRpcId | undefined {
    return typeof value === "string" || typeof value === "number" ? value : undefined;
}

function idKey(id: JsonRpcId): string {
    return `${typeof id}:${id}`;
}

function errorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
}
