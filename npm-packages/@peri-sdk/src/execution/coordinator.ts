import { provesInstance, sameTicket } from "./registry-model";
import type {
    ActivationSource, ExecutionAction, ExecutionDomainPort, ExecutionMutation, ExecutionRecord,
    ExecutionReceipt, ExecutionRegistry, ExecutionReply, ExecutionTicket, InstanceDescriptor,
    InstanceProofProvider, RunnableWork, ExecutionDisaster, AttemptRecord,
} from "./types";

export type AdmissionResult =
    | { status: "idle" }
    | { status: "running" | "settled"; ticket: ExecutionTicket }
    | { status: "disaster"; disaster: ExecutionDisaster }
    | { status: "blocked" | "unknown"; reason: string; ticket?: ExecutionTicket };

export type ExecutionCoordinatorOptions = {
    registry: ExecutionRegistry;
    domain: ExecutionDomainPort;
    instance: InstanceDescriptor;
    proofProvider: InstanceProofProvider;
    maxAttemptsPerWork: number;
    allowBestEffort?: boolean;
};

class RegistryUnavailableError extends Error {
    constructor(readonly command: ExecutionMutation, readonly outcome: "unknown" | "notApplied") {
        super(`SDK execution mutation is ${outcome}`);
    }
}

export class ExecutionCoordinator {
    private readonly active = new Map<string, { dirty: boolean; operation: Promise<AdmissionResult> }>();
    private readonly unresolved = new Map<string, ExecutionMutation>();
    private readonly instance: InstanceDescriptor;
    private sealed = false;
    seal(): void { this.sealed = true; }

    constructor(private readonly options: ExecutionCoordinatorOptions) {
        if (options.registry.durability !== "durable" && !options.allowBestEffort)
            throw new TypeError("Execution admission requires a durable registry; bestEffort must be explicitly authorized");
        if (!Number.isSafeInteger(options.maxAttemptsPerWork) || options.maxAttemptsPerWork <= 0)
            throw new TypeError("maxAttemptsPerWork must be a positive safe integer");
        this.instance = structuredClone(options.instance);
    }

    private async resolvePending(sessionId: string): Promise<void> {
        const command = this.unresolved.get(sessionId);
        if (!command) return;
        const resolution = await this.options.registry.resolve(command);
        if (resolution.status === "unknown") throw new RegistryUnavailableError(command, "unknown");
        this.unresolved.delete(sessionId);
        if (resolution.status === "notApplied") throw new RegistryUnavailableError(command, "notApplied");
    }

    private async mutate(sessionId: string, action: ExecutionAction, mutationId: string = crypto.randomUUID()): Promise<ExecutionReceipt> {
        await this.resolvePending(sessionId);
        const record = await this.options.registry.read(sessionId);
        const command: ExecutionMutation = { sessionId, mutationId, expectedRevision: record.revision, action: structuredClone(action) };
        let resolution;
        try { resolution = await this.options.registry.apply(command); }
        catch (error) { this.unresolved.set(sessionId, command); throw error; }
        if (resolution.status === "unknown") {
            this.unresolved.set(sessionId, command);
            throw new RegistryUnavailableError(command, "unknown");
        }
        if (resolution.status === "notApplied") throw new RegistryUnavailableError(command, "notApplied");
        return resolution.receipt;
    }

    async registerInstance(sessionId: string, mutationId: string): Promise<AdmissionResult> {
        if (this.sealed) return { status: "blocked", reason: "dispatcherGenerationSealed" };
        try {
            await this.resolvePending(sessionId);
            const record = await this.options.registry.read(sessionId);
            if (record.disaster) return { status: "disaster", disaster: record.disaster };
            if (this.sealed) return { status: "blocked", reason: "dispatcherGenerationSealed" };
            let previousStoppedProof;
            if (record.instance && (record.instance.instanceId !== this.instance.instanceId ||
                record.instance.generationId !== this.instance.generationId)) {
                const proof = record.instance.status === "stopped" && provesInstance(record.instance.stoppedProof, record.instance)
                    ? { status: "stopped" as const, proof: record.instance.stoppedProof! }
                    : await this.options.proofProvider.proveStopped(record.instance);
                if (proof.status !== "stopped" || !provesInstance(proof.proof, record.instance))
                    return { status: "blocked", reason: "oldInstanceNotProvenStopped" };
                previousStoppedProof = proof.proof;
            }
            const audit = await this.auditEntered(record, true);
            if (audit) return audit;
            const receipt = await this.mutate(sessionId, {
                kind: "registerInstance", instance: this.instance, previousStoppedProof,
            }, mutationId);
            if (receipt.decision.kind !== "accepted") return { status: "blocked", reason: receipt.decision.reason };
            return { status: "idle" };
        } catch (error) { return this.failure(error); }
    }

    ensureProcessing(request: { sessionId: string; source: ActivationSource }): Promise<AdmissionResult> {
        if (this.sealed) return Promise.resolve({ status: "blocked", reason: "dispatcherGenerationSealed" });
        const pending = this.active.get(request.sessionId);
        if (pending) {
            pending.dirty = true;
            return pending.operation;
        }
        const activation: { dirty: boolean; operation: Promise<AdmissionResult> } = {
            dirty: false,
            operation: Promise.resolve().then(async (): Promise<AdmissionResult> => {
                try {
                    for (let drained = 0; drained < 128; drained++) {
                        activation.dirty = false;
                        const result = await this.ensureOnce(request.sessionId);
                        const refreshable = result.status === "idle" || (result.status === "blocked" &&
                            (result.reason.startsWith("domain:") || result.reason === "domainWorkBlocked" ||
                                result.reason === "entryQualificationChanged"));
                        if (result.status !== "settled" && !(activation.dirty && refreshable)) return result;
                    }
                    return { status: "blocked" as const, reason: "activationDrainLimit" };
                } catch (error) {
                    return this.failure(error);
                } finally {
                    this.active.delete(request.sessionId);
                }
            }),
        };
        this.active.set(request.sessionId, activation);
        return activation.operation;
    }

    private failure(error: unknown): AdmissionResult {
        if (error instanceof RegistryUnavailableError)
            return { status: error.outcome === "unknown" ? "unknown" : "blocked", reason: error.message };
        return { status: "unknown", reason: error instanceof Error ? error.message : String(error) };
    }

    private owns(record: ExecutionRecord): boolean {
        return record.instance?.instanceId === this.instance.instanceId &&
            record.instance.generationId === this.instance.generationId && record.instance.status === "registered";
    }

    private ticket(sessionId: string, work: RunnableWork): ExecutionTicket {
        return {
            sessionId, admissionId: crypto.randomUUID(), instanceId: this.instance.instanceId,
            generationId: this.instance.generationId, lifecycle: work.lifecycle,
            controlGeneration: work.controlGeneration, workId: work.workId, workRevision: work.revision,
            execution: { turnId: crypto.randomUUID(), attemptId: crypto.randomUUID() },
        };
    }

    private matches(work: RunnableWork | null, ticket: ExecutionTicket): boolean {
        return work?.workId === ticket.workId && work.revision === ticket.workRevision &&
            work.lifecycle === ticket.lifecycle && work.controlGeneration === ticket.controlGeneration;
    }

    private async ensureOnce(sessionId: string): Promise<AdmissionResult> {
        if (this.sealed) return { status: "blocked", reason: "dispatcherGenerationSealed" };
        await this.resolvePending(sessionId);
        let record = await this.options.registry.read(sessionId);
        if (record.disaster) return { status: "disaster", disaster: record.disaster };
        if (!this.owns(record)) return { status: "blocked", reason: "instanceNotRegistered" };
        const audit = await this.auditEntered(record, false);
        if (audit) return audit;
        if (record.attempt && ["running", "entering", "exiting"].includes(record.attempt.phase))
            return this.recoverEntry(record.attempt.ticket);
        const query = await this.queryWork(sessionId);
        if (this.sealed) return { status: "blocked", reason: "dispatcherGenerationSealed" };
        const observed = await this.mutate(sessionId, { kind: "observeControl", control: query.control });
        if (observed.decision.kind !== "accepted") return { status: "blocked", reason: observed.decision.reason };
        record = observed.state;
        const existing = record.attempt;
        if (existing && !["settled", "blocked"].includes(existing.phase)) {
            if (existing.phase === "running") return this.recoverEntry(existing.ticket);
            if (existing.phase === "entering" || existing.phase === "exiting") return this.recoverEntry(existing.ticket);
            if (existing.invalidated) return { status: "blocked", reason: "queuedAttemptInvalidated", ticket: existing.ticket };
            return this.enter(existing.ticket);
        }
        if (query.control.status !== "active") return { status: "blocked", reason: `domain:${query.control.status}` };
        if (query.blocked) return { status: "blocked", reason: "domainWorkBlocked" };
        if (!query.work) return { status: "idle" };
        if (this.sealed) return { status: "blocked", reason: "dispatcherGenerationSealed" };
        if (query.work.lifecycle !== query.control.lifecycle || query.work.controlGeneration !== query.control.controlGeneration)
            return { status: "blocked", reason: "workControlMismatch" };
        const ticket = this.ticket(sessionId, query.work);
        const reserved = await this.mutate(sessionId, {
            kind: "reserveAttempt", ticket, maxAttempts: this.options.maxAttemptsPerWork,
        }, ticket.admissionId);
        if (reserved.decision.kind !== "accepted") return { status: "blocked", reason: reserved.decision.reason, ticket: reserved.state.attempt?.ticket };
        return this.enter(ticket);
    }

    private async enter(ticket: ExecutionTicket): Promise<AdmissionResult> {
        if (this.sealed) return { status: "blocked", reason: "dispatcherGenerationSealed", ticket };
        const query = await this.queryWork(ticket.sessionId);
        if (this.sealed) return { status: "blocked", reason: "dispatcherGenerationSealed", ticket };
        const observed = await this.mutate(ticket.sessionId, { kind: "observeControl", control: query.control });
        if (observed.decision.kind !== "accepted") return { status: "blocked", reason: observed.decision.reason, ticket };
        if (query.blocked || query.control.status !== "active" || !this.matches(query.work, ticket)) {
            await this.mutate(ticket.sessionId, { kind: "blockAttempt", ticket, reason: "entryQualificationChanged" });
            return { status: "blocked", reason: "entryQualificationChanged", ticket };
        }
        const entered = await this.mutate(ticket.sessionId, { kind: "enterAttempt", ticket });
        if (entered.decision.kind !== "accepted") return { status: "blocked", reason: entered.decision.reason, ticket };
        if (this.sealed) return { status: "blocked", reason: "dispatcherGenerationSealed", ticket };
        try { return await this.acceptReply(ticket, await this.options.domain.execute(ticket)); }
        catch (error) { return { status: "unknown", reason: `executionEntryUnknown: ${String(error)}`, ticket }; }
    }

    private async recoverEntry(ticket: ExecutionTicket): Promise<AdmissionResult> {
        const resolution = await this.options.domain.resolveExecution(ticket);
        if (resolution.status === "unknown") return { status: "unknown", reason: "executionEntryUnknown", ticket };
        if (resolution.status === "notApplied" || resolution.reply.status === "notApplied") {
            const record = await this.options.registry.read(ticket.sessionId);
            const attempt = record.attempt;
            if (!attempt || !sameTicket(attempt.ticket, ticket)) return { status: "unknown", reason: "executionRecoveryBindingMismatch", ticket };
            if (attempt.enteredEvidenceId || ["running", "exiting"].includes(attempt.phase)) {
                return this.recordDataLoss(attempt);
            }
            const receipt = await this.mutate(ticket.sessionId, {
                kind: "finishAttempt", ticket, proof: { kind: "entryNotApplied", ticket },
            });
            return receipt.decision.kind === "accepted" ? { status: "settled", ticket }
                : { status: "blocked", reason: receipt.decision.reason, ticket };
        }
        return this.acceptReply(ticket, resolution.reply);
    }

    private async recordDataLoss(attempt: AttemptRecord): Promise<AdmissionResult> {
        const disaster: ExecutionDisaster = { kind: "dataLoss", reason: "enteredAdmissionMissing", ticket: attempt.ticket,
            enteredEvidenceId: attempt.enteredEvidenceId };
        const receipt = await this.mutate(attempt.ticket.sessionId, { kind: "recordDisaster", disaster }, `${attempt.ticket.admissionId}:data-loss`);
        return receipt.state.disaster ? { status: "disaster", disaster: receipt.state.disaster }
            : { status: "unknown", reason: "disasterRecordUnconfirmed", ticket: attempt.ticket };
    }

    private async auditEntered(record: ExecutionRecord, includeLive: boolean): Promise<AdmissionResult | undefined> {
        const attempts = [...record.completedAttempts];
        if (record.attempt && (includeLive || record.attempt.phase === "settled")) attempts.push(record.attempt);
        for (const attempt of attempts) {
            if (!attempt.enteredEvidenceId && !["running", "exiting"].includes(attempt.phase)) continue;
            const resolution = await this.options.domain.resolveExecution(attempt.ticket);
            if (resolution.status === "unknown") return { status: "unknown", reason: "enteredEvidenceRecoveryUnknown", ticket: attempt.ticket };
            if (resolution.status === "notApplied" || resolution.reply.status === "notApplied") return this.recordDataLoss(attempt);
            if (!sameTicket(resolution.reply.ticket, attempt.ticket)) return { status: "unknown", reason: "enteredEvidenceReplyMismatch", ticket: attempt.ticket };
        }
    }

    verifyRecordedEntries(record: ExecutionRecord): Promise<AdmissionResult | undefined> {
        return this.auditEntered(record, true);
    }

    private async queryWork(sessionId: string) {
        let query = await this.options.domain.queryWork(sessionId);
        const pending = query.pendingCommands ?? [];
        if (pending.length) {
            for (const command of pending) {
                const original = structuredClone(command);
                if (original.sessionId !== sessionId) throw new TypeError("Pending domain command targets another Session");
                const resolution = await this.options.domain.resolveWorkCommand(original);
                if (resolution.status === "unknown") throw new Error("Original domain WorkCommand remains Unknown");
            }
            query = await this.options.domain.queryWork(sessionId);
            if (query.pendingCommands?.length) throw new Error("Original domain WorkCommand remains unresolved");
        }
        return query;
    }

    private async acceptReply(ticket: ExecutionTicket, reply: ExecutionReply): Promise<AdmissionResult> {
        if (!sameTicket(ticket, reply.ticket)) return { status: "unknown", reason: "executionReplyMismatch", ticket };
        if (reply.status === "running") {
            const existing = (await this.options.registry.read(ticket.sessionId)).attempt;
            if (existing && sameTicket(existing.ticket, ticket) && existing.phase === "running")
                return existing.invalidated ? { status: "blocked", reason: "runningAttemptAwaitingStop", ticket }
                    : { status: "running", ticket };
        }
        const action: ExecutionAction = reply.status === "settled"
            ? { kind: "finishAttempt", ticket, proof: reply.proof, executionError: reply.executionError }
            : reply.status === "notApplied"
                ? { kind: "finishAttempt", ticket, proof: { kind: "entryNotApplied", ticket } }
                : { kind: "runAttempt", ticket };
        const receipt = await this.mutate(ticket.sessionId, action);
        if (receipt.decision.kind === "accepted" && reply.status === "settled" && reply.executionError)
            return { status: "blocked", reason: `executionError:${reply.executionError.code}: ${reply.executionError.message}`, ticket };
        return receipt.decision.kind === "accepted" ? { status: reply.status === "running" ? "running" : "settled", ticket }
            : { status: "blocked", reason: receipt.decision.reason, ticket };
    }

    async complete(reply: ExecutionReply): Promise<AdmissionResult> {
        try { return await this.acceptReply(reply.ticket, reply); }
        catch (error) { return this.failure(error); }
    }
}
