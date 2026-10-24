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
    /** 工厂创建：bridge 拥有并在关闭成功时释放；声明对象：调用方借用的单次实例。 */
    owned: boolean;
    transport: AcpMcpBridgeTransport;
    connecting?: Promise<void>;
    closed: boolean;
};

/** 每次激活解析后的 acp server 绑定；构造函数不再接受原始声明。 */
export type AcpMcpBinding = {
    serverId: string;
    instance: AcpMcpServerInstance;
    owned: boolean;
};

/**
 * 同一 server 实例只能绑定一条官方 transport：官方 `Protocol.connect` 会拒绝重复连接，
 * 重连语义不在 SDK 内模拟。owned 实例关闭成功后解除绑定；借用实例保留绑定。
 */
const boundInstances = new WeakMap<object, AcpMcpBridgeTransport>();

/**
 * 反向 ACP `mcp/message` 桥接：把 Agent 声明的官方 MCP server 实例投影到 ACP 客户端请求上。
 *
 * 只处理 `{serverId, requestId, method, params}`；身份校验失败返回明确错误，不静默成功。
 * 关闭 Session 只 reject 未决请求；owned 实例由本桥接关闭，借用实例留给调用方。
 */
export class AcpMcpBridge {
    private readonly entries = new Map<string, BridgeEntry>();
    private closed = false;
    private closing?: Promise<void>;

    constructor(bindings: readonly AcpMcpBinding[]) {
        for (const binding of bindings) {
            if (typeof binding.instance?.connect !== "function")
                throw new TypeError(`MCP server "${binding.serverId}" must expose connect(transport)`);
            if (this.entries.has(binding.serverId))
                throw new Error(`Duplicate MCP serverId: ${binding.serverId}`);
            this.entries.set(binding.serverId, {
                serverId: binding.serverId,
                instance: binding.instance,
                owned: binding.owned,
                transport: new AcpMcpBridgeTransport(binding.serverId),
                closed: false,
            });
        }
    }

    /** 连接全部声明；重复调用只等待同一个连接结果。 */
    async connect(): Promise<void> {
        if (this.closed) throw new Error("MCP bridge is closed");
        await Promise.all([...this.entries.values()].map((entry) => this.connectEntry(entry)));
    }

    /**
     * 处理一条 ACP 反向 `mcp/message`，返回 ACP 响应体（`{result}` 或 `{error}`）。
     *
     * `notifications/*` 投递后立即返回空 result；普通请求等待官方 server 的响应。
     */
    async handle(params: unknown): Promise<AcpMcpResponse> {
        if (this.closed)
            return { error: { code: -32603, message: "MCP bridge is closed" } };
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

    /**
     * 一次激活结束：拒绝在途请求并关闭 transport。
     * owned 实例关闭成功才解除绑定；失败保留绑定，下一次 close 重试。
     */
    async close(reason = "ACP session ended before the MCP request completed"): Promise<void> {
        this.closed = true;
        if (this.closing) return this.closing;
        const attempt = this.closeOnce(reason).finally(() => {
            if (this.closing === attempt) this.closing = undefined;
        });
        this.closing = attempt;
        return attempt;
    }

    private async closeOnce(reason: string): Promise<void> {
        const failures: unknown[] = [];
        for (const entry of this.entries.values()) {
            entry.transport.abortPending(new Error(reason));
            try { await entry.transport.close(); }
            catch (error) { failures.push(error); }
            if (!entry.owned || entry.closed) continue;
            try { await this.closeOwnedEntry(entry); }
            catch (error) { failures.push(error); }
        }
        if (failures.length > 0)
            throw new AggregateError(failures, `MCP bridge close was not confirmed: ${reason}`);
    }

    private async closeOwnedEntry(entry: BridgeEntry): Promise<void> {
        if (entry.closed) return;
        if (typeof entry.instance.close !== "function")
            throw new TypeError(`MCP server "${entry.serverId}" is owned but has no close()`);
        await entry.instance.close();
        entry.closed = true;
        if (boundInstances.get(entry.instance) === entry.transport)
            boundInstances.delete(entry.instance);
    }

    private connectEntry(entry: BridgeEntry): Promise<void> {
        if (this.closed) return Promise.reject(new Error("MCP bridge is closed"));
        if (entry.connecting) return entry.connecting;
        const bound = boundInstances.get(entry.instance);
        if (bound && bound !== entry.transport) {
            entry.connecting = Promise.reject(new Error(entry.owned
                ? `MCP server instance for serverId "${entry.serverId}" is already bound; the factory must return a fresh instance per activation`
                : `MCP server instance for serverId "${entry.serverId}" was already used; direct acp instances are single-use—provide a factory for reuse`,
            ));
            return entry.connecting;
        }
        boundInstances.set(entry.instance, entry.transport);
        entry.connecting = Promise.resolve()
            .then(() => entry.instance.connect(entry.transport))
            .catch(async (error) => {
                const connectionError = new Error(
                    `MCP server "${entry.serverId}" connect failed: ${errorMessage(error)}`,
                    { cause: error },
                );
                // 借用实例视为已消费、保留绑定；owned 实例负责清理并在成功后解绑。
                if (!entry.owned) throw connectionError;
                const failures: unknown[] = [connectionError];
                try { await this.closeOwnedEntry(entry); }
                catch (closeError) { failures.push(closeError); }
                throw failures.length > 1
                    ? new AggregateError(failures, `MCP server "${entry.serverId}" connect and close failed`)
                    : connectionError;
            });
        return entry.connecting;
    }
}

/** 每次激活解析一次 acp 声明：工厂实例由激活拥有，直接实例是调用方借用的单次实例。 */
export async function resolveAcpMcpBindings(
    servers: Record<string, AcpMcpServer> | undefined,
): Promise<AcpMcpBinding[]> {
    const bindings: AcpMcpBinding[] = [];
    const owned: AcpMcpServerInstance[] = [];
    const seen = new Set<string>();
    try {
        for (const [name, server] of Object.entries(servers ?? {})) {
            if (server.type !== "acp") continue;
            const serverId = server.serverId ?? name;
            if (seen.has(serverId)) throw new Error(`Duplicate MCP serverId: ${serverId}`);
            seen.add(serverId);
            let instance: AcpMcpServerInstance;
            let isOwned = false;
            if (typeof server.server === "function") {
                const created = await server.server();
                if (typeof created?.close !== "function")
                    throw new TypeError(
                        `mcpServers["${name}"].server factory must return an instance with close()`,
                    );
                instance = created;
                isOwned = true;
                owned.push(created);
            } else {
                instance = server.server;
            }
            if (typeof instance?.connect !== "function")
                throw new TypeError(`mcpServers["${name}"].server must expose connect(transport)`);
            bindings.push({ serverId, instance, owned: isOwned });
        }
        return bindings;
    } catch (error) {
        const failures: unknown[] = [error];
        for (const instance of owned) {
            if (typeof instance.close !== "function") continue;
            try { await instance.close(); }
            catch (closeError) { failures.push(closeError); }
        }
        throw failures.length > 1
            ? new AggregateError(failures, "MCP server resolution failed and owned instance cleanup was not confirmed")
            : error;
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
