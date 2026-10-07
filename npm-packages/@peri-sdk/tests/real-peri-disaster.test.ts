import { expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { copyFile, mkdir, mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { StdioTransport } from "../src/transport/stdio-transport";
import { SessionExecution } from "../src/execution/session-execution";
import type { PeriConfig } from "../src/config/peri-config";

test("actual Peri entered ACK survives SDK retirement but domain SQLite backup rollback is permanent DataLoss", async () => {
    const root = await mkdtemp(join(tmpdir(), "peri-native-data-loss-"));
    const home = join(root, "home");
    await mkdir(home);
    const workspace = await realpath(root);
    const domainPath = join(root, "domain.db");
    const backupPath = join(root, "domain-before-entry.db");
    const registryPath = join(root, "registry.db");
    let modelCalls = 0;
    let releaseModel: (() => void) | undefined;
    const model = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch() {
        modelCalls++;
        await new Promise<void>((done) => { releaseModel = done; });
        return new Response('data: {"choices":[{"index":0,"delta":{"content":"reply"},"finish_reason":null}]}\n\n' +
            'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n',
            { headers: { "content-type": "text/event-stream" } });
    } });
    const settings: PeriConfig = { config: { active_alias: "sonnet",
        providers: [{ id: "fixture", type: "openai", apiKey: "fixture", baseUrl: `http://127.0.0.1:${model.port}/v1` }],
        profiles: { sonnet: { provider: "fixture", model: "fixture" } } } };
    async function start() {
        const transport = await StdioTransport.start({ command: resolve(import.meta.dir, "../../../target/debug/peri"),
            args: ["--db-path", domainPath, "acp", "--cwd", workspace, "--settings-stdin"], cwd: workspace, env: { HOME: home }, settings });
        const execution = new SessionExecution(transport, { database: registryPath });
        transport.setRequestHandler((method, params) => execution.handle(method, params));
        await transport.request("initialize", { protocolVersion: 1, clientCapabilities: {
            _meta: { "peri.executionProtocol": 1, "peri.userInputQueue": true } } });
        return { transport, execution };
    }
    const first = await start();
    let second: Awaited<ReturnType<typeof start>> | undefined;
    let firstStopped = false;
    try {
        const { sessionId } = await first.transport.request<{ sessionId: string }>("session/new", { cwd: workspace, mcpServers: [] });
        const backup = new Database(domainPath);
        backup.query("VACUUM INTO ?").run(backupPath);
        backup.close();
        const queue = await first.transport.request<{ generation: string }>("session/input/snapshot", { sessionId });
        const inputId = crypto.randomUUID();
        await first.transport.request("session/input/enqueue", { sessionId, generation: queue.generation,
            inputId, commandId: `${inputId}:enqueue`, content: "Hold the model reply", originalDraft: "Hold the model reply" });
        const activation = first.execution.activate(sessionId, "send");
        let entered = await first.execution.query(sessionId);
        for (let retry = 0; retry < 200 && (!entered.attempt?.enteredEvidenceId || modelCalls === 0); retry++) {
            await Bun.sleep(25);
            entered = await first.execution.query(sessionId);
        }
        expect(entered.attempt?.phase).toBe("running");
        expect(entered.attempt?.enteredEvidenceId).toBeTruthy();
        expect(modelCalls).toBe(1);
        const actualStore = new Database(domainPath, { readonly: true });
        const row = actualStore.query<{ state_json: string }, [string]>("SELECT state_json FROM session_work_state WHERE session_id=?").get(sessionId)!;
        expect(JSON.parse(row.state_json).admissions[entered.attempt!.ticket.admissionId]).toBeDefined();
        actualStore.close();
        first.execution.beginRetirement();
        process.kill(first.transport.pid, "SIGKILL");
        expect(await first.transport.waitForTerminationProof()).toBe(true);
        await first.transport.close();
        await activation;
        await first.execution.joinOwnedCalls();
        await first.execution.recordStopped();
        first.execution.close();
        firstStopped = true;
        await rm(`${domainPath}-wal`, { force: true });
        await rm(`${domainPath}-shm`, { force: true });
        await copyFile(backupPath, domainPath);
        second = await start();
        await second.transport.request("session/load", { sessionId, cwd: workspace, mcpServers: [] });
        const disaster = await second.execution.activate(sessionId, "recovery");
        expect(disaster).toMatchObject({ status: "disaster", disaster: { kind: "dataLoss", reason: "enteredAdmissionMissing",
            ticket: entered.attempt!.ticket, enteredEvidenceId: entered.attempt!.enteredEvidenceId } });
        const retained = await second.execution.query(sessionId);
        expect(retained.attempt?.ticket).toEqual(entered.attempt!.ticket);
        expect(retained.attempt?.enteredEvidenceId).toBe(entered.attempt!.enteredEvidenceId);
        expect(retained.instance?.status).toBe("stopped");
        expect(retained.budgets).toEqual(entered.budgets);
        expect(await second.execution.activate(sessionId, "send")).toEqual(disaster);
        expect(modelCalls).toBe(1);
    } finally {
        releaseModel?.();
        model.stop(true);
        if (second) {
            second.execution.beginRetirement();
            await second.transport.close();
            await second.execution.joinOwnedCalls();
            second.execution.close();
        }
        if (!firstStopped) {
            first.execution.beginRetirement();
            if (!await first.transport.executionStopped()) {
                try { process.kill(first.transport.pid, "SIGKILL"); }
                catch (error) { if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error; }
            }
            await first.transport.close();
            await first.execution.joinOwnedCalls();
            first.execution.close();
        }
        await rm(root, { recursive: true, force: true });
    }
}, 30000);
