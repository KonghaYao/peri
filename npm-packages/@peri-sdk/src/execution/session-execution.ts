import { mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { homedir } from "node:os";
import type { Transport } from "../transport/types";
import type { ControlState } from "../agent/session-control";
import { ExecutionAdmissionService, type AdmissionRequest, type SettlementRequest, type EntryRequest } from "./admission-service";
import { ExecutionCoordinator, type AdmissionResult } from "./coordinator";
import { AcpExecutionDomain } from "./acp-port";
import { LocalInstanceProofProvider, localHostIdentity } from "./local-proof";
import { ExecutionMutationConflictError } from "./registry-ledger";
import type { ActivationSource, ExecutionMutation, InstanceDescriptor, InstanceProofProvider } from "./types";

export type SessionExecutionOptions = {
    database?: string;
    instance?: InstanceDescriptor;
    proofProvider?: InstanceProofProvider;
    maxAttemptsPerWork?: number;
};

export class SessionExecution {
    private readonly service: ExecutionAdmissionService;
    private readonly coordinator: ExecutionCoordinator;
    private readonly registered = new Set<string>();
    private readonly registrations = new Map<string, Promise<AdmissionResult>>();
    private readonly instance: InstanceDescriptor;
    private readonly shutdownCommands = new Map<string, ExecutionMutation>();
    private phase: "accepting" | "retiring" | "retired" = "accepting";
    private readonly ownedCalls = new Set<Promise<void>>();
    constructor(private readonly transport: Transport, options: SessionExecutionOptions = {}) {
        const database = options.database ?? join(homedir(), ".peri", "execution", "registry.db");
        mkdirSync(dirname(database), { recursive: true });
        const native = transport as Transport & { pid?: number; generationId?: string; executionKind?: "wasmHost" };
        const instance = options.instance ?? {
            instanceId: crypto.randomUUID(), generationId: native.generationId ?? crypto.randomUUID(),
            proofRoute: native.pid ? { kind: "stdioSupervisor" as const, reference: "sdkLocalSqlite", hostIdentity: localHostIdentity(),
                dispatcherPid: process.pid, periPid: native.pid } : native.executionKind === "wasmHost"
                ? { kind: "wasmHost" as const, reference: native.generationId! }
                : { kind: "external" as const, reference: "customTransportRequiresProofProvider" },
        };
        const proofProvider = options.proofProvider ?? (native.pid ? new LocalInstanceProofProvider() : { proveStopped: async () => ({ status: "unknown" as const }) });
        this.instance = instance;
        this.service = new ExecutionAdmissionService({ database, instance, proofProvider, maxAttempts: options.maxAttemptsPerWork,
            verifyEntered: (record) => this.coordinator.verifyRecordedEntries(record) });
        this.coordinator = new ExecutionCoordinator({ registry: this.service.registry, domain: new AcpExecutionDomain(transport),
            instance, proofProvider, maxAttemptsPerWork: options.maxAttemptsPerWork ?? 8 });
    }
    handle(method: string, params: unknown): Promise<unknown> | undefined {
        if (method === "peri/execution/admit") {
            if (this.phase !== "accepting") return Promise.resolve({ status: "blocked", reason: "dispatcherGenerationSealed" });
            return this.track(this.service.admit(params as AdmissionRequest)).catch((error) => this.protocolFailure(error));
        }
        if (method === "peri/execution/settle") return this.track(this.service.settle(params as SettlementRequest)).catch((error) => this.protocolFailure(error));
        if (method === "peri/execution/entered") {
            if (this.phase !== "accepting") return Promise.resolve({ status: "blocked", reason: "dispatcherGenerationSealed" });
            return this.track(this.service.entered(params as EntryRequest)).catch((error) => this.protocolFailure(error));
        }
        return undefined;
    }
    private protocolFailure(error: unknown): { status: "unknown" } {
        if (error instanceof TypeError || error instanceof ExecutionMutationConflictError) throw error;
        return { status: "unknown" };
    }
    activate(sessionId: string, source: ActivationSource): Promise<AdmissionResult> {
        if (this.phase !== "accepting") return Promise.resolve({ status: "blocked", reason: "dispatcherGenerationSealed" });
        return this.track(this.activateOnce(sessionId, source));
    }
    private track<Result>(operation: Promise<Result>): Promise<Result> {
        const joined = operation.then(() => {}, () => {});
        this.ownedCalls.add(joined);
        void joined.then(() => this.ownedCalls.delete(joined));
        return operation;
    }
    private async activateOnce(sessionId: string, source: ActivationSource): Promise<AdmissionResult> {
        if (!this.registered.has(sessionId)) {
            let registration = this.registrations.get(sessionId);
            if (!registration) {
                registration = this.coordinator.registerInstance(sessionId, crypto.randomUUID());
                this.registrations.set(sessionId, registration);
            }
            const registered = await registration;
            if (this.registrations.get(sessionId) === registration) this.registrations.delete(sessionId);
            if (registered.status !== "idle") return registered;
            this.registered.add(sessionId);
            this.registrations.delete(sessionId);
        }
        return this.coordinator.ensureProcessing({ sessionId, source });
    }
    beginRetirement(): void {
        if (this.phase === "accepting") this.phase = "retiring";
        this.coordinator.seal();
    }
    async joinOwnedCalls(): Promise<void> {
        this.beginRetirement();
        while (this.ownedCalls.size) await Promise.all([...this.ownedCalls]);
        this.phase = "retired";
    }
    close(): void {
        this.beginRetirement();
        if (this.ownedCalls.size) throw new Error("Dispatcher retirement requires joining owned calls");
        this.phase = "retired";
        this.service.close();
    }
    query(sessionId: string) { return this.service.registry.read(sessionId); }
    observeControl(sessionId: string, control: ControlState) { return this.track(this.service.observeControl(sessionId, control)); }
    async recordStopped(): Promise<void> {
        if (this.phase !== "retired" || this.ownedCalls.size) throw new Error("Instance stopped proof requires sealed and joined dispatcher generation");
        const native = this.transport as Transport & { pid?: number; generationId?: string; executionStopped?: () => Promise<boolean> };
        if (native.pid !== this.instance.proofRoute.periPid || native.generationId !== this.instance.generationId || !await native.executionStopped?.())
            throw new Error("Instance stopped proof requires exact trusted transport termination evidence");
        const owned = await this.service.instanceSessions();
        for (const ownedSession of owned) await this.stopOwnedSession(ownedSession);
    }
    private async stopOwnedSession(sessionId: string): Promise<void> {
        let command = this.shutdownCommands.get(sessionId);
        const replay = command !== undefined;
        if (!command) {
            const record = await this.service.registry.read(sessionId);
            command = { sessionId, mutationId: `shutdown:${this.instance.instanceId}:${this.instance.generationId}`,
                expectedRevision: record.revision, action: { kind: "stopInstance", proof: { kind: "instanceStopped",
                instanceId: this.instance.instanceId, generationId: this.instance.generationId,
                evidenceId: `sealed-joined-dispatcher-and-verified-transport-exit:${this.instance.generationId}` } } };
            this.shutdownCommands.set(sessionId, command);
        }
        const result = replay ? await this.service.registry.resolve(command) : await this.service.registry.apply(command);
        if (result.status !== "applied" || result.receipt.decision.kind !== "accepted") throw new Error("Execution instance shutdown receipt unconfirmed");
    }
}
