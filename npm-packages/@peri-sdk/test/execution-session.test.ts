import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { SessionExecution } from "../src/execution/session-execution";
import type { AdmissionOutcome } from "../src/execution/admission-service";
import type { ExecutionTicket } from "../src/execution/types";
import type { Transport } from "../src/transport/types";
import { StdioTransport } from "../src/transport/stdio-transport";

test("SDK root executes an already reserved ticket and reverse child admission uses the same persistent registry", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-sdk-entry-"));
    const control = { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active", attempt: null };
    let finished = false;
    let executes = 0;
    let runtime: SessionExecution;
    const transport = { request: async (method: string, params: any) => {
        if (method === "session/work/query") return { control, work: finished ? null : { workId: "root-work", revision: 0, lifecycle: 1, controlGeneration: 0 } };
        if (method === "session/execute") {
            executes++;
            const ticket = params.ticket as ExecutionTicket;
            const snapshot = { sessionId: ticket.sessionId, control, blocked: false,
                candidates: [{ workId: ticket.workId, workRevision: ticket.workRevision, stage: "reasonReady", requiresRecovery: false }] };
            const confirmation = await runtime.handle("peri/execution/admit", { requestId: ticket.admissionId, snapshot, existingAdmission: ticket }) as AdmissionOutcome;
            expect(confirmation).toEqual({ status: "admitted", admission: ticket });
            const childSnapshot = { ...snapshot, sessionId: "child", candidates: [{ ...snapshot.candidates[0]!, workId: "child-work" }] };
            const child = await runtime.handle("peri/execution/admit", { requestId: "child-work-entry", snapshot: childSnapshot }) as AdmissionOutcome;
            expect(child.status).toBe("admitted");
            if (child.status !== "admitted") throw new Error("Missing child admission");
            const proof = { kind: "attemptStopped", instanceId: ticket.instanceId, generationId: ticket.generationId,
                execution: child.admission.execution, evidenceId: "confirmed-child-exit" };
            expect(await runtime.handle("peri/execution/settle", { admission: child.admission, proof }))
                .toMatchObject({ status: "applied", receipt: { evidenceId: "confirmed-child-exit" } });
            finished = true;
            return { status: "settled", ticket, proof: { ...proof, execution: ticket.execution, evidenceId: "confirmed-root-exit" } };
        }
        throw new Error(`Unexpected ${method}`);
    } } as unknown as Transport;
    runtime = new SessionExecution(transport, { database: join(directory, "registry.db"), instance: {
        instanceId: "sdk-instance", generationId: "sdk-generation", proofRoute: { kind: "external", reference: "trusted-contract-transport" },
    } });
    try {
        const results = await Promise.all([runtime.activate("root", "send"), runtime.activate("root", "notification"), runtime.activate("root", "cron")]);
        expect(results.every((result) => result.status === "idle")).toBe(true);
        expect(executes).toBe(1);
    } finally { runtime.close(); await rm(directory, { recursive: true, force: true }); }
});

test("retirement seals and joins the owned dispatcher, persists actual transport stop and permits a new generation in the same Bun host", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-sdk-retirement-"));
    const database = join(directory, "registry.db");
    const responder = `const input = require('node:readline').createInterface({input:process.stdin});
for await (const line of input) { const request = JSON.parse(line); console.log(JSON.stringify({jsonrpc:'2.0',id:request.id,result:{control:{lifecycle:1,revision:0,controlGeneration:0,status:'active',attempt:null},work:null}})); }`;
    const transport = await StdioTransport.start({ command: process.execPath, args: ["-e", responder] });
    const runtime = new SessionExecution(transport, { database });
    let replacementTransport: StdioTransport | undefined;
    let replacement: SessionExecution | undefined;
    try {
        expect(await runtime.activate("root", "recovery")).toEqual({ status: "idle" });
        const registered = await runtime.query("root");
        await expect(runtime.recordStopped()).rejects.toThrow("sealed and joined");
        runtime.beginRetirement();
        expect(await runtime.activate("root", "cron")).toMatchObject({ status: "blocked", reason: "dispatcherGenerationSealed" });
        await runtime.joinOwnedCalls();
        await expect(runtime.recordStopped()).rejects.toThrow("termination evidence");
        await transport.close();
        expect(await transport.executionStopped()).toBe(true);
        await runtime.recordStopped();
        expect((await runtime.query("root")).instance?.status).toBe("stopped");
        replacementTransport = await StdioTransport.start({ command: process.execPath, args: ["-e", responder] });
        replacement = new SessionExecution(replacementTransport, { database });
        expect(await replacement.activate("root", "recovery")).toEqual({ status: "idle" });
        const current = await replacement.query("root");
        expect(current.instance?.proofRoute.dispatcherPid).toBe(process.pid);
        expect(current.instance?.generationId).not.toBe(registered.instance?.generationId);
        expect(current.previousInstances[0]?.status).toBe("stopped");
        expect(await runtime.activate("root", "send")).toMatchObject({ status: "blocked", reason: "dispatcherGenerationSealed" });
    } finally {
        await transport.close(); await runtime.joinOwnedCalls(); runtime.close();
        await replacementTransport?.close(); await replacement?.joinOwnedCalls(); replacement?.close();
        await rm(directory, { recursive: true, force: true });
    }
}, 15000);
