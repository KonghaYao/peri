import type { JsonRpcNotification, Transport } from "../src/transport/types";

let generation = 0;

/** 最小 ACP transport：满足 Session 启动所需的 initialize/session/new|load/snapshot/close。 */
export class TestTransport implements Transport {
    readonly generationId: string;
    readonly requests: Array<{ method: string; params?: unknown }> = [];
    closeCalls = 0;
    closeError?: unknown;
    private readonly listeners = new Set<(event: JsonRpcNotification) => void>();

    constructor(
        generationId?: string,
        private readonly newSessionId = "session-new",
    ) {
        generation += 1;
        this.generationId = generationId ?? `test-generation-${generation}`;
    }

    async request<T>(method: string, params?: unknown): Promise<T> {
        this.requests.push({ method, params });
        if (method === "initialize") return { protocolVersion: 1 } as T;
        if (method === "session/new") {
            return { sessionId: this.newSessionId } as T;
        }
        if (method === "session/load") {
            return {
                sessionId: (params as { sessionId: string }).sessionId,
            } as T;
        }
        if (method === "session/input/snapshot") return { generation: "g1" } as T;
        if (method === "session/close") return {} as T;
        throw new Error(`Unexpected request: ${method}`);
    }

    async sendRequest<T>(
        method: string,
        params?: unknown,
    ): Promise<{ response: Promise<T> }> {
        return { response: this.request<T>(method, params) };
    }

    async notify(): Promise<void> {}

    subscribe(listener: (event: JsonRpcNotification) => void): () => void {
        this.listeners.add(listener);
        return () => this.listeners.delete(listener);
    }

    emit(notification: JsonRpcNotification): void {
        for (const listener of this.listeners) listener(notification);
    }

    async *events(): AsyncIterable<JsonRpcNotification> {}

    setRequestHandler(): void {}

    async close(): Promise<void> {
        this.closeCalls += 1;
        if (this.closeError) throw this.closeError;
    }
}
