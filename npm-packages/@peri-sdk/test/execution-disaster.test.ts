import { expect, test } from "bun:test";
import { ExecutionAdmissionCore, type AdmissionLedger, type AdmissionRequestRecord } from "../src/execution/admission-core";
import { ExecutionCoordinator } from "../src/execution/coordinator";
import { KvExecutionRegistry, type AtomicExecutionKv, type ExecutionKvValue } from "../src/execution/kv-registry";
import { MemoryKV } from "../src/kv/memory-kv";
import type { ExecutionAction, ExecutionMutation, ExecutionRegistry, ExecutionTicket, ExecutionDomainPort } from "../src/execution/types";

const instance = { instanceId: "rollback-instance", generationId: "rollback-generation", proofRoute: { kind: "external" as const, reference: "kv-domain-fixture" } };
const control = { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active" as const, attempt: null };
const work = { workId: "stable-work", revision: 0, lifecycle: 1, controlGeneration: 0 };

/** 部署方的 durable 准入台账：写入即持久，进程重启后仍可见。 */
class FixtureLedger implements AdmissionLedger {
    private readonly requests = new Map<string, AdmissionRequestRecord>();
    private readonly steps = new Map<string, ExecutionMutation>();
    async readRequest(requestKey: string): Promise<AdmissionRequestRecord | null> {
        return structuredClone(this.requests.get(requestKey) ?? null);
    }
    async claimRequest(requestKey: string, proposed: AdmissionRequestRecord): Promise<AdmissionRequestRecord> {
        const existing = this.requests.get(requestKey);
        if (existing) return structuredClone(existing);
        this.requests.set(requestKey, structuredClone(proposed));
        return structuredClone(proposed);
    }
    async claimStep(stepId: string, proposed: ExecutionMutation): Promise<{ inserted: boolean; command: ExecutionMutation }> {
        const existing = this.steps.get(stepId);
        if (existing) return { inserted: false, command: structuredClone(existing) };
        this.steps.set(stepId, structuredClone(proposed));
        return { inserted: true, command: structuredClone(proposed) };
    }
}

/** durable registry：与部署方的版本化 envelope 共用同一契约。 */
function durableRegistry() {
    const values = new Map<string, ExecutionKvValue>();
    const kv: AtomicExecutionKv = Object.assign(new MemoryKV(), {
        durability: "durable" as const,
        readExecutionValue: async (key: string) => structuredClone(values.get(key) ?? null),
        compareAndSetExecutionValue: async (key: string, version: number | null, next: ExecutionKvValue) => {
            if ((values.get(key)?.version ?? null) !== version) return false;
            values.set(key, structuredClone(next));
            return true;
        },
    });
    return new KvExecutionRegistry(kv);
}

/** 同一 registry 与 ledger 上重建 core/coordinator，等价于宿主重启。 */
function createPair(registry: ExecutionRegistry, ledger: AdmissionLedger, domainOf: (core: () => ExecutionAdmissionCore) => ExecutionDomainPort) {
    let core: ExecutionAdmissionCore;
    const domain = domainOf(() => core);
    core = new ExecutionAdmissionCore({ registry, ledger, instance, maxAttempts: 8,
        verifyEntered: (record) => coordinator.verifyRecordedEntries(record) });
    const coordinator = new ExecutionCoordinator({ registry: core.registry, domain, instance, maxAttemptsPerWork: 8,
        proofProvider: { proveStopped: async () => ({ status: "unknown" }) } });
    return { core, coordinator };
}

async function applyAction(registry: ExecutionRegistry, action: ExecutionAction) {
    const command: ExecutionMutation = { sessionId: "session", mutationId: crypto.randomUUID(),
        expectedRevision: (await registry.read("session")).revision, action };
    return registry.apply(command);
}

for (const phase of ["running", "exiting", "settled"] as const) {
    test(`durable entered ACK followed by a rolled-back domain admission is permanent DataLoss (${phase})`, async () => {
        const registry = durableRegistry();
        const ledger = new FixtureLedger();
        // 域侧回执：清空等价于域数据库回滚到准入之前。
        const domainReceipts = new Set<string>();
        let effects = 0;
        let admission: ExecutionTicket | undefined;
        const pair = createPair(registry, ledger, (core) => ({
            queryWork: async () => ({ control, work }),
            execute: async (ticket) => {
                admission = ticket;
                domainReceipts.add(ticket.admissionId);
                expect(await core().entered({ admission: ticket, entryEvidenceId: "fixture-domain-committed-entry" })).toMatchObject({ status: "applied" });
                effects++;
                return { status: "running", ticket };
            },
            resolveExecution: async (ticket) => domainReceipts.has(ticket.admissionId)
                ? { status: "applied", reply: { status: "running", ticket } } : { status: "notApplied" },
            resolveWorkCommand: async () => ({ status: "unknown" }),
        }));
        await pair.coordinator.registerInstance("session", "register-original");
        expect((await pair.coordinator.ensureProcessing({ sessionId: "session", source: "send" })).status).toBe("running");
        if (phase !== "running") {
            expect(await applyAction(registry, phase === "exiting" ? { kind: "exitAttempt", ticket: admission! }
                : { kind: "finishAttempt", ticket: admission!, proof: { kind: "attemptStopped", instanceId: instance.instanceId,
                    generationId: instance.generationId, execution: admission!.execution, evidenceId: "fixture-exact-attempt-joined" } }))
                .toMatchObject({ status: "applied", receipt: { decision: { kind: "accepted" } } });
        }
        const original = await registry.read("session");
        expect(original.attempt?.ticket.admissionId).toBe(admission!.admissionId);
        pair.coordinator.seal();
        domainReceipts.clear();

        const recovered = createPair(registry, ledger, () => ({
            queryWork: async () => ({ control, work }),
            execute: async () => { throw new Error("Rolled-back domain cannot admit a new execution"); },
            resolveExecution: async () => ({ status: "notApplied" }),
            resolveWorkCommand: async () => ({ status: "unknown" }),
        }));
        for (const source of ["recovery", "inboxScan", "notification", "cron", "send"] as const)
            expect(await recovered.coordinator.ensureProcessing({ sessionId: "session", source })).toMatchObject({ status: "disaster",
                disaster: { kind: "dataLoss", reason: "enteredAdmissionMissing", ticket: admission, enteredEvidenceId: "fixture-domain-committed-entry" } });
        const persisted = await registry.read("session");
        expect(persisted.attempt).toEqual(original.attempt);
        expect(persisted.budgets).toEqual(original.budgets);
        expect(effects).toBe(1);
        expect(await recovered.core.admit({ requestId: "must-not-mint", snapshot: { sessionId: "session", control, blocked: false,
            candidates: [{ workId: work.workId, workRevision: 0, stage: "reasonReady", requiresRecovery: false }] } }))
            .toMatchObject({ status: "blocked", reason: "permanentDataLoss", disaster: { kind: "dataLoss" } });
        expect(await recovered.coordinator.registerInstance("session", "must-not-replace")).toMatchObject({ status: "disaster" });
        expect([...domainReceipts]).toEqual([]);
    });
}

test("entering without entered evidence safely resolves original NotApplied without disaster or effects", async () => {
    const registry = durableRegistry();
    const ledger = new FixtureLedger();
    let executions = 0;
    let required = true;
    const domain: ExecutionDomainPort = {
        queryWork: async () => ({ control, work: required ? work : null }),
        execute: async () => { executions++; throw new Error("Entry request lost before domain apply"); },
        resolveExecution: async () => { required = false; return { status: "notApplied" }; },
        resolveWorkCommand: async () => ({ status: "unknown" }),
    };
    const coordinator = new ExecutionCoordinator({ registry, domain, instance, maxAttemptsPerWork: 8,
        proofProvider: { proveStopped: async () => ({ status: "unknown" }) } });
    await coordinator.registerInstance("session", "register");
    expect((await coordinator.ensureProcessing({ sessionId: "session", source: "send" })).status).toBe("unknown");
    expect(await coordinator.ensureProcessing({ sessionId: "session", source: "recovery" })).toEqual({ status: "idle" });
    const record = await registry.read("session");
    expect(record.disaster).toBeUndefined();
    expect(record.attempt?.stoppedProof?.kind).toBe("entryNotApplied");
    expect(record.budgets[0]?.attempts).toBe(1);
    expect(executions).toBe(1);
});
