import { createHash } from "node:crypto";
import { provesInstance, sameTicket, validateTicket } from "./registry-model";
import type { AdmissionResult } from "./coordinator";
import type { ControlState } from "./types";
import type { AttemptStoppedProof, ExecutionAction, ExecutionDisaster, ExecutionMutation, ExecutionRecord, ExecutionRegistry, ExecutionResolution, ExecutionTicket, InstanceDescriptor, InstanceProofProvider, InstanceStoppedProof } from "./types";

export type AdmissionRequestRecord = { digest: string; ticket: ExecutionTicket };
export type AdmissionStepClaim = { inserted: boolean; command: ExecutionMutation };

export interface AdmissionLedger {
    readRequest(requestKey: string): Promise<AdmissionRequestRecord | null>;
    claimRequest(requestKey: string, proposed: AdmissionRequestRecord): Promise<AdmissionRequestRecord>;
    claimStep(stepId: string, proposed: ExecutionMutation): Promise<AdmissionStepClaim>;
}

export type AdmissionSnapshot = {
    sessionId: string;
    control: ControlState;
    candidates: Array<{ workId: string; workRevision: number; stage: string; requiresRecovery: boolean }>;
    blocked: boolean;
};
export type AdmissionRequest = { requestId: string; snapshot: AdmissionSnapshot; existingAdmission?: ExecutionTicket };
export type AdmissionOutcome = { status: "admitted"; admission: ExecutionTicket }
    | { status: "busy" | "unknown" | "notApplied" } | { status: "blocked"; reason: string; disaster?: ExecutionDisaster };
export type SettlementRequest = { admission: ExecutionTicket; proof: AttemptStoppedProof };
export type SettlementOutcome = { status: "applied"; receipt: { admission: ExecutionTicket; evidenceId: string } }
    | { status: "unknown" | "notApplied" } | { status: "rejected"; reason: string };
export type EntryRequest = { admission: ExecutionTicket; entryEvidenceId: string };
export type EntryOutcome = { status: "applied"; receipt: EntryRequest }
    | { status: "unknown" | "notApplied" } | { status: "blocked"; reason: string };

function stable(value: unknown): string {
    function order(member: unknown): unknown {
        if (Array.isArray(member)) return member.map(order);
        if (member && typeof member === "object") return Object.fromEntries(Object.entries(member)
            .sort(([first], [second]) => first.localeCompare(second)).map(([key, entry]) => [key, order(entry)]));
        return member;
    }
    return JSON.stringify(order(value));
}
function digest(value: unknown): string { return createHash("sha256").update(stable(value)).digest("hex"); }

export type ExecutionAdmissionCoreOptions = {
    registry: ExecutionRegistry;
    ledger: AdmissionLedger;
    instance: InstanceDescriptor;
    maxAttempts?: number;
    allowBestEffort?: boolean;
    proofProvider?: InstanceProofProvider;
    verifyEntered?: (record: ExecutionRecord) => Promise<AdmissionResult | undefined>;
};

export class ExecutionAdmissionCore {
    readonly registry: ExecutionRegistry;
    private readonly ledger: AdmissionLedger;
    private readonly pending = new Map<string, Promise<unknown>>();
    constructor(private readonly options: ExecutionAdmissionCoreOptions) {
        if (options.registry.durability !== "durable" && !options.allowBestEffort)
            throw new TypeError("Execution admission requires a durable registry or explicit allowBestEffort");
        this.registry = options.registry;
        this.ledger = options.ledger;
    }
    private serialize<Result>(sessionId: string, operation: () => Promise<Result>): Promise<Result> {
        const result = (this.pending.get(sessionId) ?? Promise.resolve()).catch(() => {}).then(operation);
        this.pending.set(sessionId, result);
        void result.finally(() => { if (this.pending.get(sessionId) === result) this.pending.delete(sessionId); }).catch(() => {});
        return result;
    }
    private async step(sessionId: string, stepId: string, action: ExecutionAction): Promise<ExecutionResolution> {
        const current = await this.registry.read(sessionId);
        const proposed: ExecutionMutation = { sessionId, mutationId: stepId, expectedRevision: current.revision, action };
        const claimed = await this.ledger.claimStep(stepId, proposed);
        const original = claimed.command;
        if (original.sessionId !== sessionId || stable(original.action) !== stable(action)) throw new TypeError("Stable admission step identity conflict");
        return claimed.inserted ? this.registry.apply(original) : this.registry.resolve(original);
    }
    admit(request: AdmissionRequest): Promise<AdmissionOutcome> {
        const original = structuredClone(request);
        return this.serialize(original.snapshot.sessionId, () => this.admitOnce(original));
    }
    private async admitOnce(request: AdmissionRequest): Promise<AdmissionOutcome> {
        const { snapshot, requestId } = request;
        if (typeof requestId !== "string" || !requestId || requestId.length > 512) throw new TypeError("Invalid admission requestId");
        const persisted = await this.registry.read(snapshot.sessionId);
        if (persisted.disaster) return { status: "blocked", reason: "permanentDataLoss", disaster: persisted.disaster };
        const replacement = persisted.instance && (persisted.instance.instanceId !== this.options.instance.instanceId || persisted.instance.generationId !== this.options.instance.generationId);
        const evidence = [...persisted.completedAttempts, ...(replacement && persisted.attempt ? [persisted.attempt] : [])];
        if (!request.existingAdmission && evidence.some((attempt) => attempt.enteredEvidenceId || ["running", "exiting"].includes(attempt.phase))) {
            if (!this.options.verifyEntered) return { status: "blocked", reason: "enteredEvidenceRecoveryUnavailable" };
            const recovery = await this.options.verifyEntered(persisted);
            if (recovery?.status === "disaster") return { status: "blocked", reason: "permanentDataLoss", disaster: recovery.disaster };
            if (recovery) return { status: "unknown" };
        }
        if (request.existingAdmission) {
            const ticket = request.existingAdmission;
            validateTicket(ticket);
            if (ticket.sessionId !== snapshot.sessionId || requestId !== ticket.admissionId)
                return { status: "blocked", reason: "existingAdmissionIdentityMismatch" };
            const current = await this.registry.read(snapshot.sessionId);
            if (!current.attempt || !sameTicket(current.attempt.ticket, ticket)) return { status: "notApplied" };
            if (this.options.instance.instanceId !== ticket.instanceId || this.options.instance.generationId !== ticket.generationId)
                return { status: "blocked", reason: "trustedLaunchInstanceMismatch" };
            const candidate = snapshot.candidates.find((entry) => entry.workId === ticket.workId && entry.workRevision === ticket.workRevision);
            if (!candidate || snapshot.blocked || snapshot.control.status !== "active" ||
                snapshot.control.lifecycle !== ticket.lifecycle || snapshot.control.controlGeneration !== ticket.controlGeneration ||
                current.attempt.invalidated || !["entering", "running"].includes(current.attempt.phase) ||
                current.instance?.instanceId !== ticket.instanceId || current.instance.generationId !== ticket.generationId)
                return { status: "blocked", reason: "existingAdmissionNotLive" };
            return { status: "admitted", admission: ticket };
        }
        const key = digest({ sessionId: snapshot.sessionId, requestId });
        const existing = await this.ledger.readRequest(key);
        if (existing && existing.digest !== digest(request)) throw new TypeError("Admission requestId payload conflict");
        const candidate = snapshot.candidates?.[0];
        if (!existing && (snapshot.blocked || snapshot.control?.status !== "active" || !candidate))
            return { status: "blocked", reason: snapshot.blocked ? "domainWorkBlocked" : !candidate ? "noWork" : "domainNotActive" };
        const proposed: ExecutionTicket = existing ? existing.ticket : {
            sessionId: snapshot.sessionId, admissionId: key,
            instanceId: this.options.instance.instanceId, generationId: this.options.instance.generationId,
            lifecycle: snapshot.control.lifecycle, controlGeneration: snapshot.control.controlGeneration,
            workId: candidate.workId, workRevision: candidate.workRevision,
            execution: { turnId: digest({ key, kind: "turn" }).slice(0, 32).replace(/(.{8})(.{4})(.{4})(.{4})(.{12})/, "$1-$2-$3-$4-$5"), attemptId: digest({ key, kind: "attempt" }) },
        };
        validateTicket(proposed);
        const saved = await this.ledger.claimRequest(key, { digest: digest(request), ticket: proposed });
        if (saved.digest !== digest(request)) throw new TypeError("Admission requestId payload conflict");
        const ticket = saved.ticket;
        if (ticket.instanceId !== this.options.instance.instanceId || ticket.generationId !== this.options.instance.generationId)
            return { status: "blocked", reason: "originalInstanceRequired" };
        const record = await this.registry.read(snapshot.sessionId);
        let previousStoppedProof: InstanceStoppedProof | undefined;
        if (record.instance && (record.instance.instanceId !== ticket.instanceId || record.instance.generationId !== ticket.generationId)) {
            const proof = record.instance.status === "stopped" && provesInstance(record.instance.stoppedProof, record.instance)
                ? { status: "stopped" as const, proof: record.instance.stoppedProof! }
                : await this.options.proofProvider?.proveStopped(record.instance);
            if (proof?.status !== "stopped") return { status: "blocked", reason: "oldInstanceNotProvenStopped" };
            previousStoppedProof = proof.proof;
        }
        if (record.attempt && sameTicket(record.attempt.ticket, ticket) && ["entering", "running", "exiting", "settled"].includes(record.attempt.phase))
            return { status: "admitted", admission: ticket };
        if (!previousStoppedProof && record.attempt && !sameTicket(record.attempt.ticket, ticket) && !["settled", "blocked"].includes(record.attempt.phase)) return { status: "busy" };
        for (const [suffix, action] of [
            ["instance", { kind: "registerInstance", instance: this.options.instance, previousStoppedProof }],
            ["control", { kind: "observeControl", control: snapshot.control }],
            ["reserve", { kind: "reserveAttempt", ticket, maxAttempts: this.options.maxAttempts ?? 8 }],
            ["enter", { kind: "enterAttempt", ticket }],
        ] as Array<[string, ExecutionAction]>) {
            const result = await this.step(snapshot.sessionId, `${key}:${suffix}`, action);
            if (result.status !== "applied") return { status: result.status };
            if (result.receipt.decision.kind !== "accepted") return result.receipt.decision.kind === "busy"
                ? { status: "busy" } : { status: "blocked", reason: result.receipt.decision.reason };
        }
        return { status: "admitted", admission: ticket };
    }
    settle(request: SettlementRequest): Promise<SettlementOutcome> {
        request = structuredClone(request);
        return this.serialize(request.admission.sessionId, async () => {
            validateTicket(request.admission);
            if (request.proof?.kind !== "attemptStopped") return { status: "rejected", reason: "actualAttemptStoppedProofRequired" };
            const key = digest({ admission: request.admission, proof: request.proof });
            const result = await this.step(request.admission.sessionId, `settle:${key}`, { kind: "finishAttempt", ticket: request.admission, proof: request.proof });
            if (result.status !== "applied") return { status: result.status };
            return result.receipt.decision.kind === "accepted"
                ? { status: "applied", receipt: { admission: request.admission, evidenceId: request.proof.evidenceId } }
                : { status: "rejected", reason: result.receipt.decision.reason };
        });
    }
    observeControl(sessionId: string, control: ControlState): Promise<ExecutionResolution> {
        const original = structuredClone(control);
        return this.serialize(sessionId, () => this.step(sessionId, `control:${digest({ sessionId, original })}`,
            { kind: "observeControl", control: original }));
    }
    entered(request: EntryRequest): Promise<EntryOutcome> {
        request = structuredClone(request);
        return this.serialize(request.admission.sessionId, async () => {
            validateTicket(request.admission);
            if (typeof request.entryEvidenceId !== "string" || !request.entryEvidenceId || request.entryEvidenceId.length > 512)
                throw new TypeError("Invalid durable entry evidence ID");
            if (request.admission.instanceId !== this.options.instance.instanceId || request.admission.generationId !== this.options.instance.generationId)
                return { status: "blocked", reason: "trustedLaunchInstanceMismatch" };
            const record = await this.registry.read(request.admission.sessionId);
            if (!record.attempt || !sameTicket(record.attempt.ticket, request.admission)) return { status: "notApplied" };
            if (record.attempt.invalidated || !["entering", "running"].includes(record.attempt.phase))
                return { status: "blocked", reason: "entryNoLongerQualified" };
            const result = await this.step(request.admission.sessionId, `entered:${digest(request)}`, {
                kind: "runAttempt", ticket: request.admission, enteredEvidenceId: request.entryEvidenceId });
            if (result.status !== "applied") return { status: result.status };
            return result.receipt.decision.kind === "accepted" ? { status: "applied", receipt: request }
                : { status: "blocked", reason: result.receipt.decision.reason };
        });
    }
}
