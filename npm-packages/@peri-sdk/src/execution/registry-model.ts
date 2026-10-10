import { createHash } from "node:crypto";
import type {
    ExecutionMutation, ExecutionRecord, ExecutionReceipt, ExecutionTicket,
    InstanceDescriptor, InstanceStoppedProof, StoppedProof,
} from "./types";

function canonical(value: unknown): unknown {
    if (Array.isArray(value)) return value.map(canonical);
    if (value && typeof value === "object") return Object.fromEntries(
        Object.entries(value).filter((entry) => entry[1] !== undefined)
            .sort(([first], [second]) => first.localeCompare(second))
            .map(([key, member]) => [key, canonical(member)]),
    );
    return value;
}

function identity(value: string, name: string): void {
    if (typeof value !== "string" || !value || value.length > 512)
        throw new TypeError(`Invalid execution ${name}`);
}

function counter(value: number): void {
    if (!Number.isSafeInteger(value) || value < 0) throw new TypeError("Invalid execution counter");
}

export function validateTicket(ticket: ExecutionTicket): void {
    for (const field of ["sessionId", "admissionId", "instanceId", "generationId", "workId"] as const)
        identity(ticket[field], field);
    identity(ticket.execution?.turnId, "turnId");
    identity(ticket.execution?.attemptId, "attemptId");
    for (const value of [ticket.lifecycle, ticket.controlGeneration, ticket.workRevision]) counter(value);
}

export function sameTicket(first: ExecutionTicket, second: ExecutionTicket): boolean {
    return JSON.stringify(canonical(first)) === JSON.stringify(canonical(second));
}

export function mutationDigest(command: ExecutionMutation): string {
    identity(command.sessionId, "sessionId");
    identity(command.mutationId, "mutationId");
    counter(command.expectedRevision);
    const action = command.action;
    if ("ticket" in action) {
        validateTicket(action.ticket);
        if (action.ticket.sessionId !== command.sessionId) throw new TypeError("Execution Session mismatch");
    }
    if (action.kind === "registerInstance") {
        identity(action.instance.instanceId, "instanceId");
        identity(action.instance.generationId, "generationId");
        identity(action.instance.proofRoute.reference, "proofRoute");
    }
    if (action.kind === "reserveAttempt") {
        counter(action.maxAttempts);
        if (action.maxAttempts === 0) throw new TypeError("Execution budget must be positive");
    }
    if (action.kind === "observeControl") {
        for (const value of [action.control.lifecycle, action.control.revision, action.control.controlGeneration]) counter(value);
        if (!["active", "paused", "closing", "closed"].includes(action.control.status))
            throw new TypeError("Invalid domain control status");
    }
    return createHash("sha256").update(JSON.stringify(canonical(command))).digest("hex");
}

export function emptyExecutionRecord(sessionId: string): ExecutionRecord {
    identity(sessionId, "sessionId");
    return { sessionId, revision: 0, instance: null, previousInstances: [], control: null,
        attempt: null, completedAttempts: [], budgets: [] };
}

export function provesInstance(proof: InstanceStoppedProof | undefined, instance: InstanceDescriptor): boolean {
    return proof?.kind === "instanceStopped" && proof.instanceId === instance.instanceId &&
        proof.generationId === instance.generationId && typeof proof.evidenceId === "string" && proof.evidenceId.length > 0;
}

export function provesAttempt(proof: StoppedProof, ticket: ExecutionTicket): boolean {
    if (proof?.kind === "entryNotApplied") return sameTicket(proof.ticket, ticket);
    if (!proof || proof.instanceId !== ticket.instanceId || proof.generationId !== ticket.generationId || !proof.evidenceId)
        return false;
    return proof.kind === "instanceStopped" || (proof.kind === "attemptStopped" &&
        proof.execution.turnId === ticket.execution.turnId && proof.execution.attemptId === ticket.execution.attemptId);
}

function permits(record: ExecutionRecord, ticket: ExecutionTicket): boolean {
    return record.instance?.instanceId === ticket.instanceId && record.instance.generationId === ticket.generationId &&
        record.instance.status === "registered" && record.control?.status === "active" &&
        record.control.lifecycle === ticket.lifecycle && record.control.controlGeneration === ticket.controlGeneration;
}

export function decideExecution(command: ExecutionMutation, current: ExecutionRecord): ExecutionReceipt {
    mutationDigest(command);
    const state = structuredClone(current);
    const reject = (kind: "rejected" | "blocked" | "busy", reason: string): ExecutionReceipt => ({
        sessionId: command.sessionId, mutationId: command.mutationId, decision: { kind, reason }, state: structuredClone(current),
    });
    if (current.sessionId !== command.sessionId) throw new TypeError("Registry Session mismatch");
    if (current.revision !== command.expectedRevision) return reject("rejected", "staleRevision");
    if (current.revision === Number.MAX_SAFE_INTEGER) return reject("blocked", "revisionExhausted");
    const action = command.action;
    if (state.disaster && action.kind === "recordDisaster" && JSON.stringify(canonical(state.disaster)) !== JSON.stringify(canonical(action.disaster)))
        return reject("blocked", "permanentDataLoss");
    if (state.disaster && action.kind !== "stopInstance" && action.kind !== "recordDisaster" &&
        !(action.kind === "finishAttempt" && action.proof.kind !== "entryNotApplied"))
        return reject("blocked", "permanentDataLoss");
    switch (action.kind) {
        case "recordDisaster": {
            const evidence = [state.attempt, ...state.completedAttempts].find((attempt) => attempt && sameTicket(attempt.ticket, action.disaster.ticket));
            if (action.disaster.kind !== "dataLoss" || action.disaster.reason !== "enteredAdmissionMissing" ||
                !evidence || action.disaster.enteredEvidenceId !== evidence.enteredEvidenceId ||
                (!evidence.enteredEvidenceId && !["running", "exiting"].includes(evidence.phase)))
                return reject("rejected", "disasterEvidenceMismatch");
            state.disaster = action.disaster;
            break;
        }
        case "stopInstance": {
            if (!state.instance || !provesInstance(action.proof, state.instance)) return reject("blocked", "instanceNotProvenStopped");
            state.instance.status = "stopped";
            state.instance.stoppedProof = action.proof;
            if (state.attempt && state.attempt.phase !== "settled") {
                state.attempt.phase = "settled";
                state.attempt.stoppedProof = action.proof;
            }
            break;
        }
        case "registerInstance": {
            const previous = state.instance;
            if (previous?.status === "stopped" && previous.instanceId === action.instance.instanceId && previous.generationId === action.instance.generationId)
                return reject("blocked", "stoppedInstanceRequiresNewGeneration");
            if (previous && previous.instanceId === action.instance.instanceId && previous.generationId === action.instance.generationId &&
                JSON.stringify(canonical(previous.proofRoute)) !== JSON.stringify(canonical(action.instance.proofRoute)))
                return reject("blocked", "instanceProofRouteIdentityConflict");
            if (previous && (previous.instanceId !== action.instance.instanceId || previous.generationId !== action.instance.generationId)) {
                if (!provesInstance(action.previousStoppedProof, previous)) return reject("blocked", "oldInstanceNotProvenStopped");
                state.previousInstances.push({ ...previous, status: "stopped", stoppedProof: action.previousStoppedProof });
                if (state.attempt) {
                    state.completedAttempts.push({ ...state.attempt, phase: "settled", stoppedProof: action.previousStoppedProof });
                    state.attempt = null;
                }
            }
            state.instance = { ...action.instance, status: "registered" };
            break;
        }
        case "observeControl": {
            if (state.control && (action.control.lifecycle < state.control.lifecycle ||
                (action.control.lifecycle === state.control.lifecycle && action.control.revision < state.control.revision)))
                return reject("rejected", "staleControl");
            state.control = action.control;
            if (state.attempt && !permits(state, state.attempt.ticket) && state.attempt.phase !== "settled") {
                state.attempt.invalidated = true;
                state.attempt.reason = "controlChanged";
                if (state.attempt.phase === "queued") state.attempt.phase = "blocked";
            }
            break;
        }
        case "reserveAttempt": {
            if (!permits(state, action.ticket)) return reject("blocked", "domainControlOrInstanceMismatch");
            if (state.attempt && state.attempt.phase !== "settled" && state.attempt.phase !== "blocked")
                return reject("busy", "attemptAlreadyAdmitted");
            let budget = state.budgets.find((entry) => entry.workId === action.ticket.workId);
            if (budget && budget.maxAttempts !== action.maxAttempts) return reject("blocked", "budgetChangeRequiresAuthorization");
            if (budget && budget.attempts >= budget.maxAttempts) return reject("blocked", "workBudgetExhausted");
            if (!budget) { budget = { workId: action.ticket.workId, attempts: 0, maxAttempts: action.maxAttempts }; state.budgets.push(budget); }
            budget.attempts++;
            if (state.attempt) state.completedAttempts.push(state.attempt);
            state.attempt = { ticket: action.ticket, phase: "queued", invalidated: false };
            break;
        }
        case "enterAttempt":
        case "runAttempt":
        case "exitAttempt":
        case "finishAttempt":
        case "blockAttempt": {
            const attempt = state.attempt;
            if (!attempt || !sameTicket(attempt.ticket, action.ticket)) return reject("rejected", "staleAttempt");
            if (action.kind === "finishAttempt") {
                if (!provesAttempt(action.proof, action.ticket)) return reject("blocked", "attemptNotProvenStopped");
                if (action.proof.kind === "entryNotApplied" && (attempt.enteredEvidenceId || !["queued", "entering"].includes(attempt.phase)))
                    return reject("blocked", "runningAttemptRequiresStoppedProof");
                attempt.phase = "settled";
                attempt.stoppedProof = action.proof;
                if (action.executionError) {
                    if (!Number.isSafeInteger(action.executionError.code) || typeof action.executionError.message !== "string")
                        throw new TypeError("Invalid settled execution error");
                    attempt.executionError = action.executionError;
                }
            } else if (action.kind === "blockAttempt") {
                if (attempt.phase !== "queued") return reject("blocked", "enteredAttemptRequiresStoppedProof");
                attempt.phase = "blocked";
                attempt.reason = action.reason;
            } else if (action.kind === "exitAttempt") {
                if (!["entering", "running"].includes(attempt.phase)) return reject("rejected", "invalidAttemptTransition");
                attempt.phase = "exiting";
            } else {
                if (!permits(state, action.ticket) || attempt.invalidated) return reject("blocked", "controlChanged");
                const expected = action.kind === "enterAttempt" ? "queued" : "entering";
                if (attempt.phase !== expected) return reject("rejected", "invalidAttemptTransition");
                attempt.phase = action.kind === "enterAttempt" ? "entering" : "running";
                if (action.kind === "runAttempt" && action.enteredEvidenceId) attempt.enteredEvidenceId = action.enteredEvidenceId;
            }
            break;
        }
        default: throw new TypeError("Unsupported execution action");
    }
    state.revision++;
    return { sessionId: command.sessionId, mutationId: command.mutationId, decision: { kind: "accepted" }, state };
}
