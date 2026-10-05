import type { Transport } from "../transport/types";

export type ExecutionBinding = { turnId: string; attemptId: string };
export type ControlAction =
    | { kind: "stop"; target: ExecutionBinding }
    | { kind: "pause" | "resume" | "close" | "reopen" };
export type ControlCommand = {
    sessionId: string;
    commandId: string;
    expectedLifecycle: number;
    expectedRevision: number;
    expectedControlGeneration: number;
    action: ControlAction;
};
export type ControlState = {
    lifecycle: number;
    revision: number;
    controlGeneration: number;
    status: "active" | "paused" | "closing" | "closed";
    attempt: ExecutionBinding | null;
};
export type ControlRejection = "staleLifecycle" | "staleRevision" | "staleControlGeneration" |
    "staleAttempt" | "invalidTransition" | "versionExhausted";
export type ControlReceipt = {
    sessionId: string;
    commandId: string;
    decision: { kind: "accepted" } | { kind: "rejected"; reason: ControlRejection };
    state: ControlState;
};
export type ControlResolution =
    | { status: "applied"; receipt: ControlReceipt }
    | { status: "notApplied" | "unknown" };
export type ControlSnapshot = {
    state: ControlState;
    settlement: { status: "pending" | "incomplete" | "settled"; reason?: string };
};
export type CommandExpectation = Omit<ControlCommand, "sessionId" | "action">;
export type StopCommand = CommandExpectation & { target: ExecutionBinding };
export type CloseOptions = { drainTimeoutMs?: number };

export class SessionCloseUnknownError extends Error {
    constructor(readonly command: ControlCommand, cause?: unknown) {
        super("Session Close outcome is unknown; resolve the original command", { cause });
        this.name = "SessionCloseUnknownError";
    }
}

export class SessionControlNotAppliedError extends Error {
    constructor(readonly command: ControlCommand) {
        super("Control command is terminal NotApplied and cannot be replayed");
        this.name = "SessionControlNotAppliedError";
    }
}

export class SessionCloseIncompleteError extends Error {
    constructor(
        readonly receipt: ControlReceipt,
        readonly snapshot?: ControlSnapshot,
    ) {
        super(`Session close is ${snapshot?.settlement.status ?? receipt.decision.kind}: ${snapshot?.settlement.reason ?? receipt.state.status}`);
        this.name = "SessionCloseIncompleteError";
    }
}

export function controlCommandIdentity(command: ControlCommand): string {
    if (!command.sessionId || !command.commandId || command.commandId.length > 256)
        throw new TypeError("Control sessionId and commandId must be nonempty");
    if (!["stop", "pause", "resume", "close", "reopen"].includes(command.action?.kind))
        throw new TypeError("Unsupported public control action");
    for (const value of [command.expectedLifecycle, command.expectedRevision, command.expectedControlGeneration]) {
        if (!Number.isSafeInteger(value) || value < 0)
            throw new TypeError("Control expectations must be nonnegative safe integers");
    }
    if (command.action.kind === "stop" &&
        (!command.action.target?.turnId || !command.action.target?.attemptId))
        throw new TypeError("Stop requires an exact turnId and attemptId");
    return JSON.stringify([
        command.sessionId, command.commandId, command.expectedLifecycle,
        command.expectedRevision, command.expectedControlGeneration,
        command.action.kind,
        command.action.kind === "stop" ? command.action.target.turnId : null,
        command.action.kind === "stop" ? command.action.target.attemptId : null,
    ]);
}

function validateState(state: ControlState): void {
    if (!state || !["active", "paused", "closing", "closed"].includes(state.status) ||
        ![state.lifecycle, state.revision, state.controlGeneration].every((value) => Number.isSafeInteger(value) && value >= 0) ||
        (state.attempt !== null && (!state.attempt?.turnId || !state.attempt?.attemptId)))
        throw new Error("Invalid domain control state");
}

export class SessionControl {
    private readonly commands = new Map<string, string>();
    private readonly unresolved = new Set<string>();
    private readonly notApplied = new Set<string>();
    private readonly pending = new Map<string, Promise<ControlReceipt>>();
    private observed?: ControlState;

    get executionBlocked(): boolean {
        return this.unresolved.size > 0 || this.pending.size > 0 ||
            (this.observed !== undefined && this.observed.status !== "active");
    }

    isUnresolved(commandId: string): boolean {
        return this.unresolved.has(commandId) || this.pending.has(commandId);
    }

    private remember(command: ControlCommand): ControlCommand {
        const digest = controlCommandIdentity(command);
        const previous = this.commands.get(command.commandId);
        if (previous !== undefined && previous !== digest)
            throw new Error("Control commandId cannot be reused with different parameters");
        this.commands.set(command.commandId, digest);
        return structuredClone(command);
    }

    private accept(command: ControlCommand, receipt: ControlReceipt): ControlReceipt {
        if (receipt.sessionId !== command.sessionId || receipt.commandId !== command.commandId)
            throw new Error("Control receipt does not match the original command");
        if (!receipt.state || !["accepted", "rejected"].includes(receipt.decision?.kind))
            throw new Error("Invalid control receipt");
        validateState(receipt.state);
        this.unresolved.delete(command.commandId);
        if (!this.observed || receipt.state.lifecycle > this.observed.lifecycle ||
            (receipt.state.lifecycle === this.observed.lifecycle && receipt.state.revision >= this.observed.revision))
            this.observed = structuredClone(receipt.state);
        return receipt;
    }

    apply(transport: Transport, original: ControlCommand): Promise<ControlReceipt> {
        const command = this.remember(original);
        if (this.notApplied.has(command.commandId)) throw new SessionControlNotAppliedError(command);
        const pending = this.pending.get(command.commandId);
        if (pending) return pending;
        if (this.pending.size > 0)
            throw new Error("A domain control command is already in progress");
        if (this.unresolved.size > 0)
            throw new Error("Control outcome is unknown; resolve the original command first");
        const request = transport.request<ControlReceipt>("session/control", command)
            .then((receipt) => this.accept(command, receipt))
            .catch((error) => { this.unresolved.add(command.commandId); throw error; })
            .finally(() => { this.pending.delete(command.commandId); });
        this.pending.set(command.commandId, request);
        return request;
    }

    async resolve(transport: Transport, original: ControlCommand): Promise<ControlResolution> {
        const command = this.remember(original);
        if (this.pending.has(command.commandId))
            throw new Error("Wait for the original control request before resolving it");
        try {
            const resolution = await transport.request<ControlResolution>("session/control/resolve", command);
            if (resolution.status === "applied") this.accept(command, resolution.receipt);
            else if (resolution.status === "notApplied") {
                this.unresolved.delete(command.commandId);
                this.notApplied.add(command.commandId);
            }
            else if (resolution.status === "unknown") this.unresolved.add(command.commandId);
            else throw new Error("Invalid control resolution");
            return resolution;
        } catch (error) {
            this.unresolved.add(command.commandId);
            throw error;
        }
    }

    async snapshot(transport: Transport, sessionId: string): Promise<ControlSnapshot> {
        const snapshot = await transport.request<ControlSnapshot>("session/control/state", { sessionId });
        validateState(snapshot.state);
        if (!["pending", "incomplete", "settled"].includes(snapshot.settlement?.status))
            throw new Error("Invalid domain close settlement");
        if (!this.observed || snapshot.state.lifecycle > this.observed.lifecycle ||
            (snapshot.state.lifecycle === this.observed.lifecycle && snapshot.state.revision >= this.observed.revision))
            this.observed = structuredClone(snapshot.state);
        return snapshot;
    }
}
