import { expect, test } from "bun:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { ExecutionAdmissionCore, ExecutionCoordinator, KvExecutionRegistry, AcpExecutionDomain,
    ExecutionMutationConflictError, type AdmissionLedger, type AdmissionRequestRecord,
    type AdmissionStepClaim, type AdmissionSnapshot, type ExecutionMutation } from "../worker/sdk";
import { MemoryExecutionKv } from "../../@peri-sdk/src/execution/memory-registry";

class TestAdmissionLedger implements AdmissionLedger {
    readonly requests = new Map<string, AdmissionRequestRecord>();
    readonly steps = new Map<string, ExecutionMutation>();
    async readRequest(key: string): Promise<AdmissionRequestRecord | null> {
        return structuredClone(this.requests.get(key) ?? null);
    }
    async claimRequest(key: string, proposed: AdmissionRequestRecord): Promise<AdmissionRequestRecord> {
        if (!this.requests.has(key)) this.requests.set(key, structuredClone(proposed));
        return structuredClone(this.requests.get(key)!);
    }
    async claimStep(key: string, proposed: ExecutionMutation): Promise<AdmissionStepClaim> {
        const inserted = !this.steps.has(key);
        if (inserted) this.steps.set(key, structuredClone(proposed));
        return { inserted, command: structuredClone(this.steps.get(key)!) };
    }
}

const instance = { instanceId: "instance", generationId: "generation", proofRoute: { kind: "external" as const, reference: "worker" } };
function snapshot(): AdmissionSnapshot {
    return { sessionId: "session", control: { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active", attempt: null },
        candidates: [{ workId: "work", workRevision: 0, stage: "reasonReady", requiresRecovery: false }], blocked: false };
}
function fixture() {
    const ledger = new TestAdmissionLedger();
    const registry = new KvExecutionRegistry(new MemoryExecutionKv());
    return { ledger, registry, instance, allowBestEffort: true };
}

test("Core defaults to durable admission and requires explicit best-effort opt-in", () => {
    const options = fixture();
    expect(() => new ExecutionAdmissionCore({ ...options, allowBestEffort: false })).toThrow("durable registry");
    expect(new ExecutionAdmissionCore(options).registry.durability).toBe("bestEffort");
});

test("portable Core shares exact admission, entry, settlement, conflict and budget rules across reconstruction", async () => {
    const options = fixture();
    let core = new ExecutionAdmissionCore(options);
    const request = { requestId: "stable", snapshot: snapshot() };
    const [first, rival] = await Promise.all([core.admit(request), core.admit({ ...request, requestId: "rival" })]);
    expect(rival.status).toBe("busy");
    if (first.status !== "admitted") throw new Error("Expected admission");
    core = new ExecutionAdmissionCore(options);
    expect(await core.admit(request)).toEqual(first);
    const verification = { requestId: first.admission.admissionId, snapshot: snapshot(), existingAdmission: first.admission };
    expect(await core.admit(verification)).toEqual(first);
    expect(await core.admit({ ...verification, existingAdmission: { ...first.admission,
        execution: { ...first.admission.execution, attemptId: "forged" } } })).toEqual({ status: "notApplied" });
    const paused = snapshot(); paused.control.status = "paused";
    expect(await core.admit({ ...verification, snapshot: paused })).toEqual({ status: "blocked", reason: "existingAdmissionNotLive" });
    const entry = { admission: first.admission, entryEvidenceId: "durable-domain-entry" };
    expect(await core.entered(entry)).toEqual({ status: "applied", receipt: entry });
    expect(await core.entered(entry)).toEqual({ status: "applied", receipt: entry });
    expect((await core.admit(request))).toEqual(first);
    const settlement = { admission: first.admission, proof: { kind: "attemptStopped" as const, instanceId: instance.instanceId,
        generationId: instance.generationId, execution: first.admission.execution, evidenceId: "actual-host-joined" } };
    expect(await core.settle(settlement)).toMatchObject({ status: "applied" });
    core = new ExecutionAdmissionCore(options);
    expect(await core.settle(settlement)).toEqual({ status: "applied", receipt: { admission: first.admission, evidenceId: "actual-host-joined" } });
    expect(await core.admit(verification)).toEqual({ status: "blocked", reason: "existingAdmissionNotLive" });
    expect((await core.registry.read("session")).budgets[0]?.attempts).toBe(1);
    await expect(core.admit({ ...request, snapshot: { ...snapshot(), blocked: true } })).rejects.toThrow("payload conflict");
});

test("claim persisted before registry apply is resolved, never re-applied after restart", async () => {
    const options = fixture();
    const originalClaim = options.ledger.claimStep.bind(options.ledger);
    options.ledger.claimStep = async (key, command) => {
        await originalClaim(key, command);
        throw new Error("crash after durable step claim");
    };
    const request = { requestId: "lost", snapshot: snapshot() };
    await expect(new ExecutionAdmissionCore(options).admit(request)).rejects.toThrow("crash");
    const saved = structuredClone([...options.ledger.steps.values()][0]!);
    options.ledger.claimStep = originalClaim;
    expect(await new ExecutionAdmissionCore(options).admit(request)).toEqual({ status: "notApplied" });
    expect(await options.registry.resolve(saved)).toEqual({ status: "notApplied" });
    expect((await options.registry.read("session")).attempt).toBeNull();
    expect([...options.ledger.steps.values()][0]).toEqual(saved);
});

test("portable Core rejects changed ledger step identity and request claim conflicts", async () => {
    const options = fixture();
    const claim = options.ledger.claimRequest.bind(options.ledger);
    options.ledger.claimRequest = async (key, proposed) => claim(key, { ...proposed, digest: "rival-payload" });
    await expect(new ExecutionAdmissionCore(options).admit({ requestId: "conflict", snapshot: snapshot() })).rejects.toThrow("payload conflict");
    options.ledger.claimRequest = claim;
    options.ledger.claimStep = async (_key, proposed) => ({ inserted: false, command: { ...proposed, sessionId: "another-session" } });
    await expect(new ExecutionAdmissionCore(options).admit({ requestId: "step", snapshot: snapshot() })).rejects.toThrow("step identity conflict");
});

test("entered evidence audit records permanent disaster through shared coordinator rules", async () => {
    const options = fixture();
    const core = new ExecutionAdmissionCore(options);
    const admission = await core.admit({ requestId: "first", snapshot: snapshot() });
    if (admission.status !== "admitted") throw new Error("Expected admission");
    await core.entered({ admission: admission.admission, entryEvidenceId: "lost-domain-entry" });
    await core.settle({ admission: admission.admission, proof: { kind: "attemptStopped", instanceId: instance.instanceId,
        generationId: instance.generationId, execution: admission.admission.execution, evidenceId: "joined" } });
    const replacement = { ...options, instance: { ...instance, generationId: "replacement" } };
    expect(await new ExecutionAdmissionCore(replacement).admit({ requestId: "second", snapshot: snapshot() })).toEqual({ status: "blocked", reason: "enteredEvidenceRecoveryUnavailable" });
    const coordinator = new ExecutionCoordinator({ registry: options.registry, instance, allowBestEffort: true, maxAttemptsPerWork: 8,
        proofProvider: { proveStopped: async () => ({ status: "unknown" }) },
        domain: { queryWork: async () => ({ control: snapshot().control, work: null }),
            execute: async () => { throw new Error("Must not execute during audit"); },
            resolveExecution: async () => ({ status: "notApplied" }), resolveWorkCommand: async () => ({ status: "unknown" }) } });
    const recovered = new ExecutionAdmissionCore({ ...replacement, verifyEntered: (record) => coordinator.verifyRecordedEntries(record) });
    expect(await recovered.admit({ requestId: "second", snapshot: snapshot() })).toMatchObject({ status: "blocked", reason: "permanentDataLoss",
        disaster: { enteredEvidenceId: "lost-domain-entry", ticket: admission.admission } });
    expect(await new ExecutionAdmissionCore(options).admit({ requestId: "third", snapshot: snapshot() })).toMatchObject({ status: "blocked", reason: "permanentDataLoss" });
    expect((await options.registry.read("session")).budgets[0]?.attempts).toBe(1);
});

test("portable Core never takes over unproven old instances", async () => {
    const options = fixture();
    const core = new ExecutionAdmissionCore(options);
    await core.admit({ requestId: "old", snapshot: snapshot() });
    const replacement = new ExecutionAdmissionCore({ ...options, instance: { ...instance, generationId: "replacement" } });
    expect(await replacement.admit({ requestId: "replacement", snapshot: snapshot() })).toEqual({ status: "blocked", reason: "oldInstanceNotProvenStopped" });
    expect(await replacement.admit({ requestId: "old", snapshot: snapshot() })).toEqual({ status: "blocked", reason: "originalInstanceRequired" });
});

test("Workers bundle retains native crypto and executes admission, entry and settlement", async () => {
    expect(AcpExecutionDomain).toBeFunction();
    expect(ExecutionMutationConflictError).toBeFunction();
    const bundle = await Bun.build({ entrypoints: [new URL("../worker/sdk/index.ts", import.meta.url).pathname], target: "bun",
        format: "esm", external: ["node:crypto"] });
    expect(bundle.success).toBe(true);
    const source = await bundle.outputs[0]!.text();
    const directory = await mkdtemp(join(tmpdir(), "peri-workers-bundle-"));
    try {
        const path = join(directory, "workers.mjs");
        await writeFile(path, source);
        const packaged = await import(pathToFileURL(path).href) as typeof import("../worker/sdk");
        const registry = new packaged.KvExecutionRegistry(new MemoryExecutionKv());
        const core = new packaged.ExecutionAdmissionCore({ ledger: new TestAdmissionLedger(), registry, instance, allowBestEffort: true });
        const request = { requestId: "packaged", snapshot: snapshot() };
        const admission = await core.admit(request);
        if (admission.status !== "admitted") throw new Error("Expected packaged admission");
        expect(await core.admit(request)).toEqual(admission);
        expect(await core.entered({ admission: admission.admission, entryEvidenceId: "packaged-entry" })).toMatchObject({ status: "applied" });
        const settlement = { admission: admission.admission, proof: { kind: "attemptStopped" as const, instanceId: instance.instanceId,
            generationId: instance.generationId, execution: admission.admission.execution, evidenceId: "packaged-joined" } };
        expect(await core.settle(settlement)).toMatchObject({ status: "applied" });
        expect(await core.settle(settlement)).toMatchObject({ status: "applied" });
        expect((await registry.read("session")).budgets[0]?.attempts).toBe(1);
    } finally { await rm(directory, { recursive: true, force: true }); }
});
