import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ExecutionAdmissionService, type AdmissionSnapshot } from "../src/execution/admission-service";
import { localHostIdentity, LocalInstanceProofProvider } from "../src/execution/local-proof";

function snapshot(sessionId = "session"): AdmissionSnapshot {
    return { sessionId, control: { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active", attempt: null },
        candidates: [{ workId: "work", workRevision: 0, stage: "reasonReady", requiresRecovery: false }], blocked: false };
}
const instance = { instanceId: "instance", generationId: "generation", proofRoute: { kind: "external" as const, reference: "test-owner" } };

test("real SQLite admission replays, fences rival requests, validates exact tickets and persists stopped settlement", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-admission-"));
    const database = join(directory, "registry.db");
    let service = new ExecutionAdmissionService({ database, instance });
    try {
        const request = { requestId: "stable-command", snapshot: snapshot() };
        const [first, rival] = await Promise.all([service.admit(request), service.admit({ ...request, requestId: "rival" })]);
        expect(first.status).toBe("admitted");
        expect(rival.status).toBe("busy");
        if (first.status !== "admitted") throw new Error("Expected admitted");
        expect(await service.admit(request)).toEqual(first);
        expect(first.admission.execution.turnId).toMatch(/^[a-f0-9]{8}(-[a-f0-9]{4}){3}-[a-f0-9]{12}$/);
        const verification = { requestId: first.admission.admissionId, snapshot: snapshot(), existingAdmission: first.admission };
        expect(await service.admit(verification)).toEqual(first);
        expect(await service.admit({ ...verification, existingAdmission: { ...first.admission, execution: { ...first.admission.execution, attemptId: "forged" } } })).toEqual({ status: "notApplied" });
        const paused = snapshot(); paused.control.status = "paused";
        expect((await service.admit({ ...verification, snapshot: paused })).status).toBe("blocked");
        service.close(); service = new ExecutionAdmissionService({ database, instance });
        expect(await service.admit(request)).toEqual(first);
        const settlement = { admission: first.admission, proof: { kind: "attemptStopped" as const, instanceId: instance.instanceId,
            generationId: instance.generationId, execution: first.admission.execution, evidenceId: "actual-loop-exit" } };
        const receipt = await service.settle(settlement);
        expect(receipt.status).toBe("applied");
        expect(await service.settle(settlement)).toEqual(receipt);
        expect((await service.registry.read("session")).budgets[0]?.attempts).toBe(1);
        expect((await service.admit(verification)).status).toBe("blocked");
        await expect(service.admit({ ...request, snapshot: { ...snapshot(), blocked: true } })).rejects.toThrow("payload conflict");
    } finally { service.close(); await rm(directory, { recursive: true, force: true }); }
});

test("replacement without actual old instance stopped proof stays blocked", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-admission-"));
    const database = join(directory, "registry.db");
    const first = new ExecutionAdmissionService({ database, instance });
    await first.admit({ requestId: "original", snapshot: snapshot() });
    first.close();
    const replacement = new ExecutionAdmissionService({ database, instance: { ...instance, instanceId: "replacement", generationId: "new" } });
    try {
        expect(await replacement.admit({ requestId: "replacement-request", snapshot: snapshot() }))
            .toEqual({ status: "blocked", reason: "oldInstanceNotProvenStopped" });
        expect((await replacement.registry.read("session")).budgets[0]?.attempts).toBe(1);
    } finally { replacement.close(); await rm(directory, { recursive: true, force: true }); }
});

test("actual Bun JSONL sidecar advertises durable SQLite and handles readiness", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-jsonl-"));
    try {
        const frames = [{ id: "ready", method: "peri/execution/ready", params: {} },
            { id: "admit", method: "peri/execution/admit", params: { requestId: "input", snapshot: snapshot() } }];
        const child = Bun.spawn([process.execPath, join(import.meta.dir, "../src/execution/sidecar.ts"), "--database", join(directory, "registry.db"),
            "--instance-id", "jsonl-instance", "--generation-id", "jsonl-generation"], { stdin: new Blob([frames.map((frame) => JSON.stringify(frame)).join("\n") + "\n"]), stdout: "pipe", stderr: "pipe" });
        const output = (await new Response(child.stdout).text()).trim().split("\n").map((line) => JSON.parse(line));
        expect(await child.exited).toBe(0);
        expect(output.find((frame) => frame.id === "ready").result).toEqual({ protocolVersion: 1, durability: "durable" });
        expect(output.find((frame) => frame.id === "admit").result.status).toBe("admitted");
    } finally { await rm(directory, { recursive: true, force: true }); }
});

test("local instance replacement requires actual exact executor and dispatcher process exits", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-stopped-proof-"));
    const database = join(directory, "registry.db");
    const children = [0, 1].map(() => Bun.spawn([process.execPath, "-e", "await Bun.sleep(100000)"], { stdout: "ignore", stderr: "ignore" }));
    const hostIdentity = localHostIdentity();
    const owner = { ...instance, proofRoute: { kind: "stdioSupervisor" as const, reference: "localSqliteTest", hostIdentity,
        dispatcherPid: children[0]!.pid, periPid: children[1]!.pid } };
    const provider = new LocalInstanceProofProvider(hostIdentity);
    const first = new ExecutionAdmissionService({ database, instance: owner });
    await first.admit({ requestId: "old", snapshot: snapshot() });
    first.close();
    const replacement = new ExecutionAdmissionService({ database, instance: { ...instance, instanceId: "replacement" }, proofProvider: provider });
    try {
        expect(await provider.proveStopped(owner)).toEqual({ status: "unknown" });
        const request = { requestId: "replacement", snapshot: snapshot() };
        expect((await replacement.admit(request)).status).toBe("blocked");
        for (const child of children) { child.kill("SIGTERM"); await child.exited; }
        expect((await provider.proveStopped({ ...owner, proofRoute: { ...owner.proofRoute, hostIdentity: "another-host" } })).status).toBe("unknown");
        expect((await provider.proveStopped(owner)).status).toBe("stopped");
        expect((await replacement.admit(request)).status).toBe("admitted");
        const record = await replacement.registry.read("session");
        expect(record.previousInstances[0]?.status).toBe("stopped");
        expect(record.budgets[0]?.attempts).toBe(2);
    } finally {
        for (const child of children) if (child.exitCode === null) { child.kill("SIGTERM"); await child.exited; }
        replacement.close(); await rm(directory, { recursive: true, force: true });
    }
});
