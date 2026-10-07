import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Database } from "bun:sqlite";

test("packaged JSONL dispatcher runs SQLite admission and durable entry ACK without source imports", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-package-dispatcher-"));
    const database = join(directory, "registry.db");
    const child = Bun.spawn([process.execPath, join(import.meta.dir, "../dist/execution/sidecar.js"),
        "--database", database, "--instance-id", "packaged-instance", "--generation-id", "packaged-generation"],
        { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
    const reader = child.stdout.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    async function rpc(id: string, method: string, params: unknown): Promise<any> {
        await child.stdin.write(`${JSON.stringify({ id, method, params })}\n`);
        await child.stdin.flush();
        while (!buffer.includes("\n")) {
            const chunk = await reader.read();
            if (chunk.done) throw new Error(`Packaged dispatcher exited: ${await new Response(child.stderr).text()}`);
            buffer += decoder.decode(chunk.value, { stream: true });
        }
        const boundary = buffer.indexOf("\n");
        const frame = JSON.parse(buffer.slice(0, boundary));
        buffer = buffer.slice(boundary + 1);
        expect(frame.id).toBe(id);
        expect(frame.error).toBeUndefined();
        return frame.result;
    }
    try {
        expect(await rpc("ready", "peri/execution/ready", {})).toEqual({ protocolVersion: 1, durability: "durable" });
        const snapshot = { sessionId: "package-session", control: { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active", attempt: null },
            blocked: false, candidates: [{ workId: "package-work", workRevision: 0, stage: "reasonReady", requiresRecovery: false }] };
        const result = await rpc("reserve", "peri/execution/admit", { requestId: "stable-package-request", snapshot });
        expect(result.status).toBe("admitted");
        const admission = result.admission;
        expect(await rpc("entered", "peri/execution/entered", { admission, entryEvidenceId: "fixture-store-entry-committed" }))
            .toEqual({ status: "applied", receipt: { admission, entryEvidenceId: "fixture-store-entry-committed" } });
        expect(await rpc("confirm", "peri/execution/admit", { requestId: admission.admissionId, snapshot, existingAdmission: admission }))
            .toEqual({ status: "admitted", admission });
        expect(await rpc("settle", "peri/execution/settle", { admission, proof: { kind: "attemptStopped",
            instanceId: admission.instanceId, generationId: admission.generationId, execution: admission.execution, evidenceId: "fixture-exact-stop" } }))
            .toEqual({ status: "applied", receipt: { admission, evidenceId: "fixture-exact-stop" } });
        const record = await rpc("query", "peri/execution/query", { sessionId: "package-session" });
        expect(record.attempt.phase).toBe("settled");
        expect(record.budgets[0].attempts).toBe(1);
        child.stdin.end();
        expect(await child.exited).toBe(0);
        const stored = new Database(database, { readonly: true });
        expect(stored.query("SELECT session_id FROM sdk_execution_records").all()).toEqual([{ session_id: "package-session" }]);
        stored.close();
    } finally {
        reader.releaseLock();
        if (child.exitCode === null) { child.kill("SIGTERM"); await child.exited; }
        await rm(directory, { recursive: true, force: true });
    }
}, 10000);
