import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { SqliteExecutionRegistry } from "../src/execution/sqlite-registry";
import { MemoryExecutionRegistry } from "../src/execution/memory-registry";
import { ExecutionCoordinator } from "../src/execution/coordinator";
import { Database } from "bun:sqlite";
import { KvExecutionRegistry, type AtomicExecutionKv, type ExecutionKvValue } from "../src/execution/kv-registry";
import { MemoryKV } from "../src/kv/memory-kv";
import type { DomainWorkCommand, ExecutionAction, ExecutionMutation, ExecutionRegistry, ExecutionTicket, ExecutionDomainPort } from "../src/execution/types";

const instance = { instanceId: "instance", generationId: "generation", proofRoute: { kind: "external" as const, reference: "owner" } };
const control = { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active" as const, attempt: null };
const ticket: ExecutionTicket = { sessionId: "session", admissionId: "admission", instanceId: "instance", generationId: "generation",
    lifecycle: 1, controlGeneration: 0, workId: "work", workRevision: 0, execution: { turnId: crypto.randomUUID(), attemptId: "attempt" } };
async function apply(registry: ExecutionRegistry, action: ExecutionAction, mutationId = crypto.randomUUID()) {
    const command: ExecutionMutation = { sessionId: "session", mutationId, expectedRevision: (await registry.read("session")).revision, action };
    return { command, result: await registry.apply(command) };
}
async function contract(registry: ExecutionRegistry) {
    const registered = await apply(registry, { kind: "registerInstance", instance });
    expect(await registry.apply(registered.command)).toEqual(registered.result);
    await expect(registry.apply({ ...registered.command, expectedRevision: 99 })).rejects.toThrow("mutationId");
    const sealed: ExecutionMutation = { sessionId: "session", mutationId: "sealed", expectedRevision: 1, action: { kind: "observeControl", control } };
    expect(await registry.resolve(sealed)).toEqual({ status: "notApplied" });
    expect(await registry.apply(sealed)).toEqual({ status: "notApplied" });
    await apply(registry, { kind: "observeControl", control });
    const reserved = await apply(registry, { kind: "reserveAttempt", ticket, maxAttempts: 1 });
    expect(reserved.result.status).toBe("applied");
    await apply(registry, { kind: "enterAttempt", ticket });
    await apply(registry, { kind: "observeControl", control: { ...control, revision: 1, controlGeneration: 1, status: "paused" } });
    const state = await registry.read("session");
    expect(state.attempt?.phase).toBe("entering"); expect(state.attempt?.invalidated).toBe(true);
    const replacement = await apply(registry, { kind: "registerInstance", instance: { ...instance, instanceId: "new" } });
    expect(replacement.result).toMatchObject({ status: "applied", receipt: { decision: { kind: "blocked", reason: "oldInstanceNotProvenStopped" } } });
    const stopped = { kind: "attemptStopped" as const, instanceId: "instance", generationId: "generation", execution: ticket.execution, evidenceId: "actual-stop" };
    await apply(registry, { kind: "finishAttempt", ticket, proof: stopped });
    await apply(registry, { kind: "observeControl", control: { ...control, revision: 2, controlGeneration: 2 } });
    const retry = await apply(registry, { kind: "reserveAttempt", ticket: { ...ticket, admissionId: "retry", controlGeneration: 2 }, maxAttempts: 1 });
    expect(retry.result).toMatchObject({ status: "applied", receipt: { decision: { reason: "workBudgetExhausted" } } });
    expect((await registry.read("session")).budgets[0]?.attempts).toBe(1);
}

test("memory CAS follows contract but explicitly remains bestEffort", async () => {
    const registry = new MemoryExecutionRegistry();
    expect(registry.durability).toBe("bestEffort"); await contract(registry);
    expect(() => new ExecutionCoordinator({ registry, instance, maxAttemptsPerWork: 1,
        domain: {} as ExecutionDomainPort, proofProvider: { proveStopped: async () => ({ status: "unknown" }) } })).toThrow("bestEffort");
});
test("actual SQLite persists CAS receipts and lifecycle invalidation without resetting stable-work budget", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-registry-"));
    const registry = new SqliteExecutionRegistry({ path: join(directory, "registry.db") });
    try { await contract(registry); }
    finally { registry.close(); await rm(directory, { recursive: true, force: true }); }
});
test("atomic KV adapter uses one versioned envelope for the same receipt and lifecycle contract", async () => {
    const values = new Map<string, ExecutionKvValue>();
    const kv: AtomicExecutionKv = Object.assign(new MemoryKV(), {
        durability: "bestEffort" as const,
        readExecutionValue: async (key: string) => structuredClone(values.get(key) ?? null),
        compareAndSetExecutionValue: async (key: string, version: number | null, next: ExecutionKvValue) => {
            if ((values.get(key)?.version ?? null) !== version) return false;
            values.set(key, structuredClone(next));
            return true;
        },
    });
    const registry = new KvExecutionRegistry(kv);
    await contract(registry);
    expect(values.size).toBe(1);
    expect([...values.keys()]).toEqual(["peri:sdk-execution:session:session"]);
    const envelope = values.get("peri:sdk-execution:session:session")!;
    values.set("peri:sdk-execution:session:session", { ...envelope, protocolVersion: 2 } as unknown as ExecutionKvValue);
    await expect(registry.read("session")).rejects.toThrow("protocol version");
});
test("unsupported persistent SQLite registry version fails closed rather than recreating records", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-registry-version-"));
    const path = join(directory, "registry.db");
    try {
        const registry = new SqliteExecutionRegistry({ path });
        await apply(registry, { kind: "registerInstance", instance });
        registry.close();
        const database = new Database(path);
        database.exec("UPDATE sdk_execution_registry_meta SET protocol_version=2");
        database.close();
        expect(() => new SqliteExecutionRegistry({ path })).toThrow("protocol version");
        const persisted = new Database(path, { readonly: true });
        expect(persisted.query("SELECT session_id FROM sdk_execution_records").all()).toEqual([{ session_id: "session" }]);
        persisted.close();
    } finally { await rm(directory, { recursive: true, force: true }); }
});
test("coordinator Unknown entry freezes and resolves the exact original ticket, not a second execute", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-coordinator-"));
    const registry = new SqliteExecutionRegistry({ path: join(directory, "registry.db") });
    const executions: ExecutionTicket[] = [];
    const resolutions: ExecutionTicket[] = [];
    const domain: ExecutionDomainPort = {
        queryWork: async () => ({ control, work: { workId: "work", revision: 0, lifecycle: 1, controlGeneration: 0 } }),
        execute: async (admission) => { executions.push(admission); throw new Error("reply lost"); },
        resolveExecution: async (admission) => { resolutions.push(admission); return { status: "unknown" }; },
        resolveWorkCommand: async () => ({ status: "unknown" }),
    };
    const coordinator = new ExecutionCoordinator({ registry, domain, instance, maxAttemptsPerWork: 2,
        proofProvider: { proveStopped: async () => ({ status: "unknown" }) } });
    try {
        await coordinator.registerInstance("session", "instance-registration");
        expect((await coordinator.ensureProcessing({ sessionId: "session", source: "send" })).status).toBe("unknown");
        for (const source of ["inboxScan", "notification", "recovery", "cron"] as const)
            expect((await coordinator.ensureProcessing({ sessionId: "session", source })).status).toBe("unknown");
        expect(executions).toHaveLength(1);
        expect(resolutions).toHaveLength(4);
        expect(resolutions.every((admission) => JSON.stringify(admission) === JSON.stringify(executions[0]))).toBe(true);
        expect((await registry.read("session")).budgets[0]?.attempts).toBe(1);
        expect((await registry.read("session")).attempt?.phase).toBe("entering");
    } finally { registry.close(); await rm(directory, { recursive: true, force: true }); }
});

test("pending domain mutation resolves its complete original command before any attempt is admitted", async () => {
    const registry = new MemoryExecutionRegistry();
    const command: DomainWorkCommand = { sessionId: "session", recipientLifecycle: 1, mutationId: "original-publication",
        action: { kind: "publish", expectedRevision: 37, expectedControlGeneration: 9, inputId: "original-input" } };
    const resolved: DomainWorkCommand[] = [];
    let pending = true;
    let uncertain = true;
    const domain: ExecutionDomainPort = {
        queryWork: async () => ({ control, work: null, pendingCommands: pending ? [command] : [] }),
        resolveWorkCommand: async (original) => {
            resolved.push(original);
            if (uncertain) return { status: "unknown" };
            pending = false;
            return { status: "applied", receipt: { mutationId: command.mutationId } };
        },
        execute: async () => { throw new Error("No admission before domain recovery"); },
        resolveExecution: async () => ({ status: "unknown" }),
    };
    const coordinator = new ExecutionCoordinator({ registry, domain, instance, allowBestEffort: true, maxAttemptsPerWork: 8,
        proofProvider: { proveStopped: async () => ({ status: "unknown" }) } });
    await coordinator.registerInstance("session", "register");
    expect((await coordinator.ensureProcessing({ sessionId: "session", source: "recovery" })).status).toBe("unknown");
    expect((await registry.read("session")).budgets).toEqual([]);
    uncertain = false;
    expect((await coordinator.ensureProcessing({ sessionId: "session", source: "recovery" })).status).toBe("idle");
    expect(resolved).toEqual([command, command]);
    expect((await registry.read("session")).attempt).toBeNull();
});

test("actual stopped executionError persists failure without draining the stable-work budget", async () => {
    const registry = new MemoryExecutionRegistry();
    let executions = 0;
    const domain: ExecutionDomainPort = {
        queryWork: async () => ({ control, work: { workId: "work", revision: 0, lifecycle: 1, controlGeneration: 0 } }),
        execute: async (admission) => {
            executions++;
            return { status: "settled", ticket: admission,
                proof: { kind: "attemptStopped", instanceId: admission.instanceId, generationId: admission.generationId,
                    execution: admission.execution, evidenceId: "joined-exact-attempt" },
                executionError: { code: -32000, message: "Actual root observer rejected entry" } };
        },
        resolveExecution: async () => ({ status: "unknown" }),
        resolveWorkCommand: async () => ({ status: "unknown" }),
    };
    const coordinator = new ExecutionCoordinator({ registry, domain, instance, allowBestEffort: true, maxAttemptsPerWork: 8,
        proofProvider: { proveStopped: async () => ({ status: "unknown" }) } });
    await coordinator.registerInstance("session", "register");
    expect(await coordinator.ensureProcessing({ sessionId: "session", source: "send" }))
        .toMatchObject({ status: "blocked", reason: "executionError:-32000: Actual root observer rejected entry" });
    expect(executions).toBe(1);
    const record = await registry.read("session");
    expect(record.attempt).toMatchObject({ phase: "settled", executionError: { code: -32000 } });
    expect(record.budgets[0]?.attempts).toBe(1);
});
