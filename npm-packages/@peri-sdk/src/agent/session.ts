import type { Agent } from "./agent";
import { SendReceipt } from "./send-receipt";
import type { JsonRpcNotification, Transport } from "../transport/types";
import type { SessionDocs } from "../state/session-docs";

type QueueSnapshot = {
    generation: string;
    revision?: number;
    activeRequestId?: string;
    items?: Array<{ inputId: string; state: string }>;
};
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
    private currentPath?: string;
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
    private readonly pendingInputText = new Map<string, string>();

    constructor(private readonly agent: Agent) {}

    get id(): string {
        if (!this.sessionId) throw new Error("Session has not started");
        return this.sessionId;
    }

    get docs(): SessionDocs {
        return this.agent.docs;
    }

    get path(): string | undefined { return this.currentPath; }

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
            const path = requestedSessionId === null
                ? this.agent.path
                : (await this.agent.options.sandbox.getSession(requestedSessionId))?.cwd;
            if (!path) throw new Error(requestedSessionId === null
                ? "Agent path is required for a new Session"
                : `Session not found: ${requestedSessionId}`);
            transport = await this.agent.options.sandbox.createTransport(path);
            transport.setRequestHandler((method, params) =>
                this.docs.handleRequest(method, params, this.sessionId, this.agent.options),
            );
            let replayTurnPending = false;
            this.unsubscribe = transport.subscribe((event) => {
                const rawEvent =
                    event.method === "session/update" ||
                    event.method === "peri/agent_event";
                const stateEvent =
                    rawEvent ||
                    event.method === "peri/agent_event_done" ||
                    event.method === "peri/unstable_event";
                if (!stateEvent) return;
                if (
                    this.sessionId &&
                    (event.params as { sessionId?: string } | undefined)
                        ?.sessionId !== this.sessionId
                )
                    return;
                if (event.method === "session/update") {
                    const update = (event.params as { update?: { sessionUpdate?: string; _meta?: { periReplay?: boolean } } } | undefined)?.update;
                    if (
                        update?.sessionUpdate === "user_message_chunk" ||
                        update?.sessionUpdate === "agent_message_chunk" ||
                        update?.sessionUpdate === "agent_thought_chunk" ||
                        update?.sessionUpdate === "tool_call" ||
                        update?.sessionUpdate === "tool_call_update"
                    ) {
                        replayTurnPending = update._meta?.periReplay === true;
                    }
                } else if (event.method === "peri/agent_event_done") {
                    replayTurnPending = false;
                }
                try {
                    this.docs.accept(event);
                } catch {
                    // State projection must not suppress delivery handling or the raw ACP stream.
                    console.error("Failed to project ACP notification into SessionDocs");
                }
                if (!rawEvent) return;
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
                        "peri.agentEventDone": true,
                        "peri.replay": true,
                        "peri.tokenStats": true,
                        "peri.planEntryActiveForm": true,
                        "peri.unstableEvent": true,
                    },
                },
            });
            const mcpServers = this.agent.mcpServers();
            const params = {
                cwd: path,
                mcpServers,
            };
            let id: string;
            let ownsExecution = true;
            if (requestedSessionId === null) {
                const response = await transport.request<{ sessionId: string; modes?: unknown; configOptions?: unknown[] }>(
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
                this.docs.seedConfig(response);
                await claims.claimSession(id);
            } else {
                const loaded = await transport.request<{ modes?: unknown; configOptions?: unknown[]; _meta?: { "peri.sessionWorkspaceV1"?: { read_only?: unknown } } }>("session/load", {
                    ...params,
                    sessionId: requestedSessionId,
                });
                const identity = loaded._meta?.["peri.sessionWorkspaceV1"];
                this.docs.seedConfig(loaded);
                ownsExecution = identity !== undefined && identity.read_only === undefined;
                // ACP completes history replay before the load response. It has no
                // replay turn-done notification, so close only a replay-only turn.
                if (replayTurnPending) this.docs.completeTurn();
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
            this.docs.seedInputQueue(snapshot);
            this.transport = transport;
            this.sessionId = id;
            this.currentPath = path;
            this.generation = snapshot.generation;
            this.docs.setTaskSnapshotRequester(() => this.transport!.request("session/bg-tasks", { sessionId: id }));
            this.state = "active";
            return this;
        } catch (error) {
            this.unsubscribe?.();
            this.unsubscribe = undefined;
            if (transport) await transport.close().catch(() => {});
            this.agent.discardFailedSessionDocs();
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

    trackInput(inputId: string, text: string): void {
        this.pendingInputText.set(inputId, text);
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
        const text = this.pendingInputText.get(inputId);
        if (text) this.docs.acceptDeliveredUserInput(inputId, text);
        this.pendingInputText.delete(inputId);
        this.pendingDeliveries.delete(inputId);
        pending.resolve();
    }

    rejectDelivery(inputId: string, error: unknown): void {
        this.pendingInputText.delete(inputId);
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

    async cancel(): Promise<void> {
        const transport = this.activeTransport();
        const token = this.docs.requestCancel();
        try {
            await transport.notify("session/cancel", { sessionId: this.id });
        } catch (error) {
            if (token) this.docs.restoreCancel(token);
            throw error;
        }
    }

    async close(): Promise<void> {
        if (this.state === "closed") return;
        if (this.state === "starting") await this.startPromise?.catch(() => {});
        if (this.state === "active" && this.transport) {
            this.docs.setTaskSnapshotRequester(null);
            await this.transport.close();
            await this.agent.claims.release();
        }
        this.docs.completeTurn("cancelled");
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
