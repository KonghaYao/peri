import { expect, test } from "bun:test";
import { createServer } from "node:net";
import { once } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { TursoExecutionRegistry } from "../src/execution/turso-registry";
import type { ExecutionMutation } from "../src/execution/types";

test("real local Turso listener atomically persists execution CAS and stable mutation receipts", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-execution-turso-"));
    const listener = createServer(); listener.listen(0, "127.0.0.1"); await once(listener, "listening");
    const address = listener.address(); if (!address || typeof address === "string") throw new Error("Missing listener address");
    await new Promise<void>((resolve) => listener.close(() => resolve()));
    const url = `http://127.0.0.1:${address.port}`;
    const server = Bun.spawn(["mise", "exec", "--", "sqld", "--no-welcome", "--http-listen-addr", `127.0.0.1:${address.port}`,
        "--db-path", join(directory, "database")], { stdout: "ignore", stderr: "pipe" });
    let first: TursoExecutionRegistry | undefined;
    let second: TursoExecutionRegistry | undefined;
    try {
        let ready = false;
        for (let attempt = 0; attempt < 100; attempt++) {
            if (server.exitCode !== null) throw new Error(`sqld exited: ${await new Response(server.stderr).text()}`);
            try { await fetch(`${url}/health`); ready = true; break; } catch { await Bun.sleep(50); }
        }
        if (!ready) throw new Error("sqld did not become ready");
        first = new TursoExecutionRegistry({ url }); second = new TursoExecutionRegistry({ url });
        const command: ExecutionMutation = { sessionId: "session", mutationId: "stable", expectedRevision: 0,
            action: { kind: "registerInstance", instance: { instanceId: "instance", generationId: "generation",
                proofRoute: { kind: "external", reference: "remote-owner-requires-proof" } } } };
        const receipt = await first.apply(command);
        expect(receipt).toMatchObject({ status: "applied", receipt: { decision: { kind: "accepted" } } });
        expect(await second.resolve(command)).toEqual(receipt);
        await expect(second.apply({ ...command, expectedRevision: 1 })).rejects.toThrow("mutationId");
        const sealed = { ...command, mutationId: "never-applied", expectedRevision: 1 };
        expect(await second.resolve(sealed)).toEqual({ status: "notApplied" });
        expect(await first.apply(sealed)).toEqual({ status: "notApplied" });
        expect((await second.read("session")).revision).toBe(1);
    } finally {
        await first?.close(); await second?.close();
        server.kill("SIGTERM"); await server.exited;
        await rm(directory, { recursive: true, force: true });
    }
}, 15000);
