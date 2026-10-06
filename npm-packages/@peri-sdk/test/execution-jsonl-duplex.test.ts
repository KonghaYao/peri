import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ExecutionTicket } from "../src/execution/types";

test("real Bun SQLite dispatcher remains full duplex during root execute reverse confirmation", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-jsonl-duplex-"));
    const child = Bun.spawn([process.execPath, join(import.meta.dir, "../src/execution/sidecar.ts"),
        "--database", join(directory, "registry.db"), "--instance-id", "duplex-instance", "--generation-id", "duplex-generation"],
        { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
    const reader = child.stdout.getReader();
    let buffered = "";
    const decoder = new TextDecoder();
    async function frame(): Promise<any> {
        while (!buffered.includes("\n")) {
            const { value, done } = await reader.read();
            if (done) throw new Error("Unexpected dispatcher EOF");
            buffered += decoder.decode(value, { stream: true });
        }
        const boundary = buffered.indexOf("\n");
        const line = buffered.slice(0, boundary); buffered = buffered.slice(boundary + 1);
        return JSON.parse(line);
    }
    async function send(value: unknown): Promise<void> { await child.stdin.write(`${JSON.stringify(value)}\n`); await child.stdin.flush(); }
    const control = { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active", attempt: null };
    let execution: ExecutionTicket | undefined;
    let executionRpcId: string | undefined;
    let finished = false;
    let settledReply: unknown;
    try {
        await send({ id: "activate", method: "peri/execution/activate", params: { sessionId: "root", source: "recovery" } });
        for (;;) {
            const message = await frame();
            if (message.method === "session/work/query") {
                await send({ id: message.id, result: { control, work: finished ? null : { workId: "work", revision: 0, lifecycle: 1, controlGeneration: 0 } } });
            } else if (message.method === "session/execute") {
                execution = message.params.ticket; executionRpcId = message.id;
                await send({ id: "confirm", method: "peri/execution/admit", params: { requestId: execution!.admissionId, existingAdmission: execution,
                    snapshot: { sessionId: "root", control, blocked: false,
                        candidates: [{ workId: "work", workRevision: 0, stage: "reasonReady", requiresRecovery: false }] } } });
            } else if (message.id === "confirm") {
                expect(message.result).toEqual({ status: "admitted", admission: execution });
                await send({ id: "entered", method: "peri/execution/entered", params: { admission: execution, entryEvidenceId: "trusted-fixture-entry-committed" } });
            } else if (message.id === "entered") {
                expect(message.result).toEqual({ status: "applied", receipt: { admission: execution, entryEvidenceId: "trusted-fixture-entry-committed" } });
                finished = true;
                settledReply = { status: "settled", ticket: execution, proof: { kind: "attemptStopped",
                    instanceId: execution!.instanceId, generationId: execution!.generationId,
                    execution: execution!.execution, evidenceId: "trusted-fixture-exit" } };
                await send({ id: executionRpcId, result: settledReply });
            } else if (message.method === "session/execute/resolve") {
                expect(message.params.ticket).toEqual(execution);
                await send({ id: message.id, result: { status: "applied", reply: settledReply } });
            } else if (message.id === "activate") {
                expect(message.result).toEqual({ status: "idle" }); break;
            } else throw new Error(`Unexpected dispatcher frame: ${JSON.stringify(message)}`);
        }
        await send({ id: "record", method: "peri/execution/query", params: { sessionId: "root" } });
        const record = await frame();
        expect(record.result.budgets[0].attempts).toBe(1);
        expect(record.result.attempt.phase).toBe("settled");
        child.stdin.end(); expect(await child.exited).toBe(0);
    } finally {
        reader.releaseLock();
        if (child.exitCode === null) { child.kill("SIGTERM"); await child.exited; }
        await rm(directory, { recursive: true, force: true });
    }
}, 10000);
