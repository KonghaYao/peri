import type { Agent } from "./agent";
import { SendReceipt } from "./send-receipt";
import type { JsonRpcNotification, Transport } from "../transport/types";
import { SessionExecution } from "../execution/session-execution";
import { ExecutionDataLossError, EXECUTION_PROTOCOL_VERSION } from "../execution/types";
import type { ActivationSource } from "../execution/types";
import type { AdmissionResult } from "../execution/coordinator";
import type { SessionDocs } from "../state/session-docs";
import { EventQueue } from "../transport/event-queue";
import {
    SessionControl, controlCommandIdentity,
    type CommandExpectation, type StopCommand, type ControlAction,
    type ControlCommand, type ControlReceipt, type ControlResolution, type ControlSnapshot, type CloseOptions,
} from "./session-control";
import { drainClose, closeDrainBudget } from "./close-drain";

type QueueSnapshot = {
    generation: string;
    revision?: number;
    activeRequestId?: string;
    items?: Array<{ inputId: string; state: string }>;
};
type QueueReceipt = { results: Array<{ inputId: string; state: string }>; workReceipts?: Array<{ decision: { kind: string } }> };
type SessionState = "declared" | "starting" | "active" | "cleanup-pending" | "closed";
type PendingDelivery = {
    resolve: () => void;
    reject: (error: Error) => void;
};

export class Session {
    private state: SessionState = "declared";
    private transport?: Transport;
    private execution?: SessionExecution;
    private sessionId?: string;
    private currentPath?: string;
    private generation?: string;
    private startPromise?: Promise<Session>;
    private closePromise?: Promise<ControlReceipt>;
    private startupCleanupPromise?: Promise<void>;
    private readonly controls = new SessionControl();
    private settledClose?: { command: ControlCommand; receipt: ControlReceipt };
    private notifications = new EventQueue(() => {});
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
    get isClosed(): boolean { return this.state === "closed"; }

    start(requestedSessionId: string | null): Promise<Session> {
        if (this.closePromise || this.startupCleanupPromise)
            throw new Error("Session cannot start while closing");
        if (this.state !== "declared")
            throw new Error(`Session cannot start from ${this.state}`);
        if (requestedSessionId === "")
            return Promise.reject(new TypeError("Session id must be nonempty or null"));
        this.state = "starting";
        this.startPromise = this.startOnce(requestedSessionId);
        return this.startPromise;
    }

    private async startOnce(
        requestedSessionId: string | null,
    ): Promise<Session> {
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
            this.transport = transport;
            transport.setRequestHandler((method, params) => {
                if ((method === "peri/execution/admit" || method === "peri/execution/entered") && (this.closePromise || this.controls.executionBlocked))
                    return { status: "blocked", reason: "sessionDomainControlBlocksAdmission" };
                if (method === "peri/execution/admit" || method === "peri/execution/settle" || method === "peri/execution/entered")
                    return this.executionRuntime().handle(method, params);
                return this.docs.handleRequest(method, params, this.sessionId, this.agent.options);
            });
            let replayTurnPending = false;
            this.unsubscribe = transport.subscribe((event) => {
                if (event.method === "session/work/available") {
                    const notice = event.params as { sessionId?: string; executionProtocol?: number } | undefined;
                    if (notice?.executionProtocol !== EXECUTION_PROTOCOL_VERSION) {
                        console.error("Unsupported Peri work activation protocol");
                        return;
                    }
                    const sessionId = notice.sessionId;
                    if (sessionId && sessionId === this.sessionId) void this.ensureProcessing("notification").catch((error) => {
                        console.error("SDK work admission failed", error);
                    });
                    return;
                }
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
                } catch (error) {
                    // State projection must not suppress delivery handling or the raw ACP stream.
                    console.error("Failed to project ACP notification into SessionDocs", error);
                }
                if (!rawEvent) return;
                this.acceptDeliveryEvent(event);
                this.notifications.push(event);
            });
            const initialized = await transport.request<{ agentCapabilities?: { _meta?: Record<string, unknown> } }>("initialize", {
                protocolVersion: 1,
                clientCapabilities: {
                    _meta: {
                        "peri.userInputQueue": true,
                        "peri.executionProtocol": EXECUTION_PROTOCOL_VERSION,
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
            if (initialized.agentCapabilities?._meta?.["peri.executionProtocol"] !== EXECUTION_PROTOCOL_VERSION)
                throw new TypeError("Peri execution protocol version 2 capability is required");
            const mcpServers = this.agent.mcpServers();
            const params = {
                cwd: path,
                mcpServers,
            };
            let id: string;
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
                const loaded = await transport.request<{ modes?: unknown; configOptions?: unknown[] }>("session/load", {
                    ...params,
                    sessionId: requestedSessionId,
                });
                this.docs.seedConfig(loaded);
                // ACP completes history replay before the load response. It has no
                // replay turn-done notification, so close only a replay-only turn.
                if (replayTurnPending) this.docs.completeTurn();
                id = requestedSessionId;
            }
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
            this.state = "cleanup-pending";
            this.unsubscribe?.();
            this.unsubscribe = undefined;
            await this.notifications.return();
            this.notifications = new EventQueue(() => {});
            this.agent.discardFailedSessionDocs();
            try {
                await this.cleanupExecution();
            } catch (cleanupError) {
                throw new AggregateError(
                    [error, cleanupError],
                    `Session startup failed: ${String(error)}; cleanup failed: ${String(cleanupError)}. Retry cleanupStartup before starting another Session.`,
                    { cause: error },
                );
            }
            this.state = "declared";
            throw error;
        }
    }

    private async cleanupExecution(): Promise<void> {
        if (this.transport) {
            await this.transport.close();
            await this.execution?.joinOwnedCalls();
            const native = this.transport as Transport & { executionStopped?: () => Promise<boolean> };
            if (this.execution && this.sessionId && this.settledClose && await native.executionStopped?.())
                await this.execution.recordStopped();
            this.execution?.close();
            this.execution = undefined;
            this.transport = undefined;
        }
        await this.agent.claims.release();
    }

    private activeTransport(): Transport {
        if (this.closePromise || this.state !== "active" || !this.transport)
            throw new Error("Session is not active");
        if (this.controls.executionBlocked)
            throw new Error("Session execution is blocked by domain control");
        return this.transport;
    }

    send(text: string): SendReceipt {
        this.activeTransport();
        if (!text) throw new TypeError("Cannot send empty user input");
        return new SendReceipt(this, text);
    }

    private executionRuntime(): SessionExecution {
        if (!this.execution) this.execution = new SessionExecution(this.activeTransport(), this.agent.options.execution);
        return this.execution;
    }

    ensureProcessing(source: ActivationSource = "inboxScan"): Promise<AdmissionResult> {
        if (this.closePromise || this.state !== "active" || this.controls.executionBlocked)
            return Promise.resolve({ status: "blocked", reason: "sessionDomainControlBlocksAdmission" });
        return this.executionRuntime().activate(this.id, source);
    }

    trackInput(inputId: string, text: string): void {
        this.pendingInputText.set(inputId, text);
    }

    waitForDelivery(inputId: string): Promise<void> {
        return new Promise((resolve, reject) =>
            this.pendingDeliveries.set(inputId, {
                resolve, reject,
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
            };
        };
        const value = payload.value;
        if (!value) return;
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
                commandId: `${inputId}:enqueue`,
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
        if (state !== "delivered") this.requirePublication(receipt);
        if (state !== "delivered") this.activatePublishedInput(inputId, "send");
        return state;
    }

    async dispatch(inputId: string): Promise<void> {
        const receipt = await this.activeTransport().request<QueueReceipt>(
            "session/input/dispatch",
            {
                sessionId: this.id,
                generation: this.generation,
                commandId: `${inputId}:dispatch`,
                inputIds: [inputId],
            },
        );
        const state = receipt.results.find(
            (item) => item.inputId === inputId,
        )?.state;
        if (!state || state === "unknown" || state === "withdrawn")
            throw new Error(`Input not dispatched: ${state ?? "missing"}`);
        if (state !== "delivered") this.requirePublication(receipt);
        this.activatePublishedInput(inputId, "send");
    }

    private activatePublishedInput(inputId: string, source: ActivationSource): void {
        if (this.closePromise || this.state !== "active" || this.controls.executionBlocked) return;
        void this.ensureProcessing(source).then((result) => {
            if (result.status === "disaster") this.rejectDelivery(inputId, new ExecutionDataLossError(result.disaster));
            else if (result.status === "unknown" || result.status === "blocked")
                this.rejectDelivery(inputId, new Error(`Execution admission ${result.status}: ${result.reason}`));
        }, (error) => this.rejectDelivery(inputId, error));
    }

    private requirePublication(receipt: QueueReceipt): void {
        if (!receipt.workReceipts?.length || receipt.workReceipts.some((work) => work.decision?.kind !== "accepted"))
            throw new Error("Required input publication has no verified accepted domain receipt");
    }

    async takeBack(inputId: string): Promise<void> {
        await this.activeTransport().request("session/input/takeback", {
            sessionId: this.id,
            generation: this.generation,
            commandId: `${inputId}:takeback`,
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
                const next = await this.notifications.next();
                if (next.done) break;
                const event = next.value;
                if (
                    event &&
                    (event.params as { sessionId?: string } | undefined)
                        ?.sessionId === this.id
                )
                    yield event;
            }
        } finally {
            this.streamOpen = false;
            await this.notifications.return();
            if (!this.streamEnded) this.notifications = new EventQueue(() => {});
        }
    }

    private controlTransport(): Transport {
        if (this.state !== "active" || !this.transport)
            throw new Error("Session has no active domain transport");
        return this.transport;
    }

    private command(expectation: CommandExpectation, action: ControlAction): ControlCommand {
        return { ...expectation, sessionId: this.id, action };
    }

    async control(command: ControlCommand): Promise<ControlReceipt> {
        if (this.closePromise) throw new Error("Session domain Close is in progress");
        if (command.sessionId !== this.id)
            throw new Error("Control command targets a different Session");
        const receipt = await this.controls.apply(this.controlTransport(), command);
        if (receipt.decision.kind === "accepted" && this.execution) await this.execution.observeControl(this.id, receipt.state);
        return receipt;
    }

    stop({ target, ...expectation }: StopCommand): Promise<ControlReceipt> {
        return this.control(this.command(expectation, { kind: "stop", target }));
    }

    pause(expectation: CommandExpectation): Promise<ControlReceipt> {
        return this.control(this.command(expectation, { kind: "pause" }));
    }

    resume(expectation: CommandExpectation): Promise<ControlReceipt> {
        return this.control(this.command(expectation, { kind: "resume" }));
    }

    async reopen(expectation: CommandExpectation): Promise<ControlReceipt> {
        const receipt = await this.control(this.command(expectation, { kind: "reopen" }));
        if (receipt.decision.kind === "accepted") {
            const snapshot = await this.controlTransport().request<QueueSnapshot>("session/input/snapshot", { sessionId: this.id });
            if (!snapshot.generation) throw new Error("Reopen has no verified input publication generation");
            this.generation = snapshot.generation;
            this.docs.seedInputQueue(snapshot);
        }
        return receipt;
    }

    resolveControl(command: ControlCommand): Promise<ControlResolution> {
        if (command.sessionId !== this.id)
            throw new Error("Control command targets a different Session");
        return this.controls.resolve(this.controlTransport(), command);
    }

    controlState(): Promise<ControlSnapshot> {
        return this.controls.snapshot(this.controlTransport(), this.id);
    }

    close(expectation: CommandExpectation, options: CloseOptions = {}): Promise<ControlReceipt> {
        if (!expectation) throw new TypeError("Domain Close requires a stable command");
        const drainOptions = { ...options };
        closeDrainBudget(drainOptions);
        const command = this.command(expectation, { kind: "close" });
        if (this.settledClose) {
            if (controlCommandIdentity(command) !== controlCommandIdentity(this.settledClose.command))
                throw new Error("Transport shutdown can only retry the settled Close command");
            if (this.state === "closed") return Promise.resolve(this.settledClose.receipt);
        }
        if (!this.closePromise) {
            this.closingCommand = controlCommandIdentity(command);
            this.closePromise = Promise.resolve().then(() => this.closeOnce(command, drainOptions)).finally(() => {
                this.closePromise = undefined;
            });
        } else if (this.closingCommand !== controlCommandIdentity(command)) {
            throw new Error("A different Close command is already in progress");
        }
        return this.closePromise;
    }

    private closingCommand?: string;

    private async closeOnce(command: ControlCommand, options: CloseOptions): Promise<ControlReceipt> {
        const receipt = this.settledClose?.receipt ?? await drainClose(command, {
            apply: () => this.controls.apply(this.controlTransport(), command),
            resolve: () => this.controls.resolve(this.controlTransport(), command),
            snapshot: () => this.controlState(),
            isUnresolved: () => this.controls.isUnresolved(command.commandId),
        }, options);
        if (!this.settledClose) {
            this.settledClose = { command, receipt };
        }
        this.execution?.beginRetirement();
        this.state = "cleanup-pending";
        this.docs.setTaskSnapshotRequester(null);
        await this.cleanupExecution();
        this.docs.completeTurn("cancelled");
        this.unsubscribe?.();
        this.streamEnded = true;
        for (const inputId of this.pendingDeliveries.keys()) {
            this.rejectDelivery(
                inputId,
                new Error("Session closed before user input was delivered"),
            );
        }
        await this.notifications.return();
        this.state = "closed";
        return receipt;
    }

    cleanupStartup(): Promise<void> {
        if (this.sessionId || this.state === "active" || this.state === "closed")
            throw new Error("Startup cleanup cannot shut down an established Session");
        if (this.state === "starting")
            throw new Error("Wait for Session startup to finish before cleanupStartup");
        if (!this.startupCleanupPromise) {
            this.startupCleanupPromise = this.cleanupExecution().then(() => {
                this.state = "declared";
            }).finally(() => { this.startupCleanupPromise = undefined; });
        }
        return this.startupCleanupPromise;
    }
}
