import { expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { copyFile, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ExecutionAdmissionService } from "../src/execution/admission-service";
import { ExecutionCoordinator } from "../src/execution/coordinator";
import type { ExecutionDomainPort, ExecutionMutation, ExecutionTicket } from "../src/execution/types";

const instance = { instanceId: "rollback-instance", generationId: "rollback-generation", proofRoute: { kind: "external" as const, reference: "sqlite-domain-fixture" } };
const control = { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active" as const, attempt: null };
const work = { workId: "stable-work", revision: 0, lifecycle: 1, controlGeneration: 0 };

test.each(["running", "exiting", "settled"] as const)("SQLite domain backup rollback after durable entered ACK in %s is permanent DataLoss", async (phase) => {
    const directory = await mkdtemp(join(tmpdir(), "peri-data-loss-"));
    const sdkPath = join(directory, "sdk.db");
    const domainPath = join(directory, "domain.db");
    const backupPath = join(directory, "domain-before-entry.db");
    let domainDatabase = new Database(domainPath, { create: true });
    domainDatabase.exec("CREATE TABLE domain_admission_receipts(admission_id TEXT PRIMARY KEY,ticket_json TEXT NOT NULL)");
    domainDatabase.query("VACUUM INTO ?").run(backupPath);
    let service = new ExecutionAdmissionService({ database: sdkPath, instance });
    let effects = 0;
    let admission: ExecutionTicket | undefined;
    const domain: ExecutionDomainPort = {
        queryWork: async () => ({ control, work }),
        execute: async (ticket) => {
            admission = ticket;
            domainDatabase.query("INSERT INTO domain_admission_receipts VALUES (?,?)").run(ticket.admissionId, JSON.stringify(ticket));
            expect(await service.entered({ admission: ticket, entryEvidenceId: "fixture-domain-committed-entry" })).toMatchObject({ status: "applied" });
            effects++;
            return { status: "running", ticket };
        },
        resolveExecution: async (ticket) => domainDatabase.query("SELECT 1 FROM domain_admission_receipts WHERE admission_id=?").get(ticket.admissionId)
            ? { status: "applied", reply: { status: "running", ticket } } : { status: "notApplied" },
        resolveWorkCommand: async () => ({ status: "unknown" }),
    };
    const options = { domain, instance, maxAttemptsPerWork: 8, proofProvider: { proveStopped: async () => ({ status: "unknown" as const }) } };
    try {
        const first = new ExecutionCoordinator({ ...options, registry: service.registry });
        await first.registerInstance("session", "register-original");
        expect((await first.ensureProcessing({ sessionId: "session", source: "send" })).status).toBe("running");
        if (phase !== "running") {
            const record = await service.registry.read("session");
            const command: ExecutionMutation = { sessionId: "session", mutationId: `fixture-${phase}`, expectedRevision: record.revision,
                action: phase === "exiting" ? { kind: "exitAttempt", ticket: admission! } : { kind: "finishAttempt", ticket: admission!,
                    proof: { kind: "attemptStopped", instanceId: instance.instanceId, generationId: instance.generationId,
                        execution: admission!.execution, evidenceId: "fixture-exact-attempt-joined" } } };
            expect(await service.registry.apply(command)).toMatchObject({ status: "applied", receipt: { decision: { kind: "accepted" } } });
        }
        const original = await service.registry.read("session");
        first.seal();
        service.close();
        domainDatabase.close();
        await copyFile(backupPath, domainPath);
        domainDatabase = new Database(domainPath);
        service = new ExecutionAdmissionService({ database: sdkPath, instance });
        const recovered = new ExecutionCoordinator({ ...options, registry: service.registry });
        for (const source of ["recovery", "inboxScan", "notification", "cron", "send"] as const)
            expect(await recovered.ensureProcessing({ sessionId: "session", source })).toMatchObject({ status: "disaster",
                disaster: { kind: "dataLoss", reason: "enteredAdmissionMissing", ticket: admission, enteredEvidenceId: "fixture-domain-committed-entry" } });
        const persisted = await service.registry.read("session");
        expect(persisted.attempt).toEqual(original.attempt);
        expect(persisted.budgets).toEqual(original.budgets);
        expect(effects).toBe(1);
        expect(await service.admit({ requestId: "must-not-mint", snapshot: { sessionId: "session", control, blocked: false,
            candidates: [{ workId: work.workId, workRevision: 0, stage: "reasonReady", requiresRecovery: false }] } }))
            .toMatchObject({ status: "blocked", reason: "permanentDataLoss", disaster: { kind: "dataLoss" } });
        expect(await recovered.registerInstance("session", "must-not-replace")).toMatchObject({ status: "disaster" });
        expect(domainDatabase.query("SELECT * FROM domain_admission_receipts").all()).toEqual([]);
    } finally { service.close(); domainDatabase.close(); await rm(directory, { recursive: true, force: true }); }
});

test("SQLite entering without entered evidence safely resolves original NotApplied without disaster or effects", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-never-entered-"));
    const service = new ExecutionAdmissionService({ database: join(directory, "sdk.db"), instance });
    let executions = 0;
    let required = true;
    const domain: ExecutionDomainPort = {
        queryWork: async () => ({ control, work: required ? work : null }),
        execute: async () => { executions++; throw new Error("Entry request lost before domain apply"); },
        resolveExecution: async () => { required = false; return { status: "notApplied" }; },
        resolveWorkCommand: async () => ({ status: "unknown" }),
    };
    const coordinator = new ExecutionCoordinator({ registry: service.registry, domain, instance, maxAttemptsPerWork: 8,
        proofProvider: { proveStopped: async () => ({ status: "unknown" }) } });
    try {
        await coordinator.registerInstance("session", "register");
        expect((await coordinator.ensureProcessing({ sessionId: "session", source: "send" })).status).toBe("unknown");
        expect(await coordinator.ensureProcessing({ sessionId: "session", source: "recovery" })).toEqual({ status: "idle" });
        const record = await service.registry.read("session");
        expect(record.disaster).toBeUndefined();
        expect(record.attempt?.stoppedProof?.kind).toBe("entryNotApplied");
        expect(record.budgets[0]?.attempts).toBe(1);
        expect(executions).toBe(1);
    } finally { service.close(); await rm(directory, { recursive: true, force: true }); }
});
