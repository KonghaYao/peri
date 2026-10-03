import type { Agent } from "./agent";
import { SendReceipt } from "./send-receipt";
import type { JsonRpcNotification, Transport } from "../transport/types";

type QueueSnapshot = { generation: string };
type QueueReceipt = { results: Array<{ inputId: string; state: string }> };
type SessionState = "declared" | "starting" | "active" | "closed";
type PendingDelivery = {
    resolve: () => void;
    reject: (error: Error) => void;
    retry: () => Promise<void>;
    wasDispatching: boolean;
    wasClaimed: boolean;
    retries: number;
};

export class Session {
    private state: SessionState = "declared";
    private transport?: Transport;
    private sessionId?: string;
    private generation?: string;
    private startPromise?: Promise<Session>;
    private readonly notifications: JsonRpcNotification[] = [];
    private readonly notificationWaiters: Array<
        (event: JsonRpcNotification | null) => void
    > = [];
    private unsubscribe?: () => void;
    private streamOpen = false;
    private streamEnded = false;
    private readonly pendingDeliveries = new Map<string, PendingDelivery>();

    constructor(private readonly agent: Agent) {}

    get id(): string {
        if (!this.sessionId) throw new Error("Session has not started");
        return this.sessionId;
    }

    start(requestedSessionId: string | null): Promise<Session> {
        if (this.state !== "declared")
            throw new Error(`Session cannot start from ${this.state}`);
        this.state = "starting";
        this.startPromise = this.startOnce(requestedSessionId).catch(
            (error) => {
                this.state = "declared";
                throw error;
            },
        );
        return this.startPromise;
    }

    private async startOnce(
        requestedSessionId: string | null,
    ): Promise<Session> {
        if (requestedSessionId === "")
            throw new TypeError("Session id must be nonempty or null");
        const claims = this.agent.claims;
        let transport: Transport | undefined;
        try {
            await claims.claimAgent();
            if (requestedSessionId !== null)
                await claims.claimSession(requestedSessionId);
            transport = await this.agent.options.sandbox.createTransport();
            this.unsubscribe = transport.subscribe((event) => {
                if (
                    event.method !== "session/update" &&
                    event.method !== "peri/agent_event"
                )
                    return;
                if (
                    this.sessionId &&
                    (event.params as { sessionId?: string } | undefined)
                        ?.sessionId !== this.sessionId
                )
                    return;
                this.acceptDeliveryEvent(event);
                const waiter = this.notificationWaiters.shift();
                if (waiter) waiter(event);
                else this.notifications.push(event);
            });
            await transport.request("initialize", {
                protocolVersion: 1,
                clientCapabilities: {
                    _meta: {
                        "peri.userInputQueue": true,
                        "peri.agentEvent": true,
                        "peri.sessionWorkspaceV1": true,
                    },
                },
            });
            const mcpServers = this.agent.mcpServers();
            const params = {
                cwd: this.agent.options.sandbox.path,
                mcpServers,
            };
            let id: string;
            let ownsExecution = true;
            if (requestedSessionId === null) {
                const response = await transport.request<{ sessionId: string }>(
                    "session/new",
                    {
                        ...params,
                        ...(this.agent.options.instructions
                            ? {
                                  _meta: {
                                      "peri.instructions":
                                          this.agent.options.instructions,
                                  },
                              }
                            : {}),
                    },
                );
                id = response.sessionId;
                if (!id) throw new Error("Peri returned no sessionId");
                await claims.claimSession(id);
            } else {
                const loaded = await transport.request<{ _meta?: { "peri.sessionWorkspaceV1"?: { read_only?: unknown } } }>("session/load", {
                    ...params,
                    sessionId: requestedSessionId,
                });
                const identity = loaded._meta?.["peri.sessionWorkspaceV1"];
                ownsExecution = identity !== undefined && identity.read_only === undefined;
                id = requestedSessionId;
            }
            if (ownsExecution)
                await this.agent.options.sandbox.registerSessionTransport(id, transport);
            const snapshot = await transport.request<QueueSnapshot>(
                "session/input/snapshot",
                { sessionId: id },
            );
            if (!snapshot.generation)
                throw new Error("Peri returned no input queue generation");
            this.transport = transport;
            this.sessionId = id;
            this.generation = snapshot.generation;
            this.state = "active";
            return this;
        } catch (error) {
            this.unsubscribe?.();
            this.unsubscribe = undefined;
            if (transport) await transport.close().catch(() => {});
            await claims.release();
            throw error;
        }
    }

    private activeTransport(): Transport {
        if (this.state !== "active" || !this.transport)
            throw new Error("Session is not active");
        return this.transport;
    }

    send(text: string): SendReceipt {
        this.activeTransport();
        if (!text) throw new TypeError("Cannot send empty user input");
        return new SendReceipt(this, text);
    }

    waitForDelivery(inputId: string, retry: () => Promise<void>): Promise<void> {
        return new Promise((resolve, reject) =>
            this.pendingDeliveries.set(inputId, {
                resolve, reject, retry,
                wasDispatching: false, wasClaimed: false, retries: 0,
            }),
        );
    }

    confirmDelivery(inputId: string): void {
        const pending = this.pendingDeliveries.get(inputId);
        if (!pending) return;
        this.pendingDeliveries.delete(inputId);
        pending.resolve();
    }

    rejectDelivery(inputId: string, error: unknown): void {
        const pending = this.pendingDeliveries.get(inputId);
        if (!pending) return;
        this.pendingDeliveries.delete(inputId);
        pending.reject(
            error instanceof Error ? error : new Error(String(error)),
        );
    }

    private acceptDeliveryEvent(event: JsonRpcNotification): void {
        if (event.method !== "peri/agent_event") return;
        const eventJson = (event.params as { event_json?: unknown } | undefined)
            ?.event_json;
        if (typeof eventJson !== "string") return;
        let parsed: unknown;
        try {
            parsed = JSON.parse(eventJson);
        } catch {
            return;
        }
        if (!parsed || typeof parsed !== "object") return;
        const payload = parsed as {
            type?: string;
            value?: {
                input_id?: string;
                generation?: string;
                snapshot?: {
                    generation?: string;
                    items?: Array<{ inputId: string; state: string }>;
                };
            };
        };
        const value = payload.value;
        if (!value) return;
        if (payload.type === "user_input_queue_changed") {
            const snapshot = value.snapshot;
            const items = snapshot?.items;
            if (snapshot?.generation !== this.generation || !Array.isArray(items)) return;
            for (const [inputId, pending] of this.pendingDeliveries) {
                const state = items.find((item) => item.inputId === inputId)?.state;
                if (state === "dispatching") pending.wasDispatching = true;
                else if (state === "claimed") pending.wasClaimed = true;
                else if (state === "queued" && pending.wasDispatching) {
                    pending.wasDispatching = false;
                    if (!pending.wasClaimed && pending.retries++ === 0) {
                        void pending.retry().catch((error) => this.rejectDelivery(inputId, error));
                    } else {
                        void this.takeBack(inputId).then(
                            () => this.rejectDelivery(inputId, new Error("Input execution failed before delivery")),
                            () => this.rejectDelivery(inputId, new Error("Input execution failed; queued input could not be withdrawn")),
                        );
                    }
                }
            }
            return;
        }
        if (
            payload.type === "user_input_delivered" &&
            value.generation === this.generation &&
            typeof value.input_id === "string"
        )
            this.confirmDelivery(value.input_id);
    }

    async enqueue(inputId: string, text: string): Promise<string> {
        const receipt = await this.activeTransport().request<QueueReceipt>(
            "session/input/enqueue",
            {
                sessionId: this.id,
                generation: this.generation,
                commandId: crypto.randomUUID(),
                inputId,
                content: text,
                originalDraft: text,
            },
        );
        const state = receipt.results.find(
            (item) => item.inputId === inputId,
        )?.state;
        if (!state || state === "unknown" || state === "withdrawn")
            throw new Error(`Input not accepted: ${state ?? "missing"}`);
        return state;
    }

    async dispatch(inputId: string): Promise<void> {
        const receipt = await this.activeTransport().request<QueueReceipt>(
            "session/input/dispatch",
            {
                sessionId: this.id,
                generation: this.generation,
                commandId: crypto.randomUUID(),
                inputIds: [inputId],
            },
        );
        const state = receipt.results.find(
            (item) => item.inputId === inputId,
        )?.state;
        if (!state || state === "unknown" || state === "withdrawn")
            throw new Error(`Input not dispatched: ${state ?? "missing"}`);
    }

    private async takeBack(inputId: string): Promise<void> {
        await this.activeTransport().request("session/input/takeback", {
            sessionId: this.id,
            generation: this.generation,
            commandId: crypto.randomUUID(),
            inputId,
        });
    }

    /** Raw ACP notifications for this Session: session/update and peri/agent_event. */
    async *stream(): AsyncIterable<JsonRpcNotification> {
        this.activeTransport();
        if (this.streamOpen)
            throw new Error("This Session already has an event consumer");
        this.streamOpen = true;
        try {
            while (!this.streamEnded) {
                const event =
                    this.notifications.shift() ??
                    (await new Promise<JsonRpcNotification | null>(
                        (resolve) => {
                            this.notificationWaiters.push(resolve);
                        },
                    ));
                if (
                    event &&
                    (event.params as { sessionId?: string } | undefined)
                        ?.sessionId === this.id
                )
                    yield event;
            }
        } finally {
            this.streamOpen = false;
        }
    }

    cancel(): Promise<void> {
        return this.activeTransport().notify("session/cancel", {
            sessionId: this.id,
        });
    }

    async close(): Promise<void> {
        if (this.state === "closed") return;
        if (this.state === "starting") await this.startPromise?.catch(() => {});
        if (this.state === "active" && this.transport) {
            await this.transport.close();
            await this.agent.claims.release();
        }
        this.unsubscribe?.();
        this.streamEnded = true;
        for (const inputId of this.pendingDeliveries.keys()) {
            this.rejectDelivery(
                inputId,
                new Error("Session closed before user input was delivered"),
            );
        }
        for (const waiter of this.notificationWaiters.splice(0)) waiter(null);
        this.state = "closed";
    }
}
