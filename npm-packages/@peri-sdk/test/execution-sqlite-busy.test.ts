import { expect, spyOn, test } from "bun:test";
import { Database, SQLiteError } from "bun:sqlite";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ExecutionAdmissionService } from "../src/execution/admission-service";
import { isSqliteBusy, retrySqliteBusy, sqliteImmediate } from "../src/execution/sqlite-busy";
import { SqliteExecutionRegistry } from "../src/execution/sqlite-registry";
import type { ExecutionMutation } from "../src/execution/types";

const instance = { instanceId: "busy-instance", generationId: "busy-generation",
    proofRoute: { kind: "external" as const, reference: "isolated-writer" } };
const control = { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active" as const, attempt: null };
const command: ExecutionMutation = { sessionId: "session", mutationId: "stable-mutation", expectedRevision: 0,
    action: { kind: "registerInstance", instance } };

async function releaseWriter<Result>(writer: Database, operation: () => Promise<Result>) {
    writer.exec("BEGIN IMMEDIATE");
    let settled = false;
    const result = operation().finally(() => { settled = true; });
    const released = new Promise<boolean>((resolve, reject) => {
        setTimeout(() => {
            const settledBeforeRelease = settled;
            try { writer.exec("COMMIT"); resolve(settledBeforeRelease); }
            catch (error) { reject(error); }
        }, 0);
    });
    const [outcome, settledBeforeRelease] = await Promise.all([result, released]);
    expect(settledBeforeRelease).toBe(false);
    return outcome;
}

test.each(["apply", "resolve"] as const)("real independent SQLite writer yields during registry %s and preserves stable resolution", async (operation) => {
    const directory = await mkdtemp(join(tmpdir(), "peri-registry-busy-"));
    const path = join(directory, "registry.db");
    const registry = new SqliteExecutionRegistry({ path });
    const writer = new Database(path);
    try {
        const result = await releaseWriter(writer, () => registry[operation](command));
        if (operation === "apply") {
            expect(result).toMatchObject({ status: "applied", receipt: { decision: { kind: "accepted" } } });
            expect(await registry.resolve(command)).toEqual(result);
            expect((await registry.read("session")).revision).toBe(1);
        } else {
            expect(result).toEqual({ status: "notApplied" });
            expect(await registry.apply(command)).toEqual({ status: "notApplied" });
            expect((await registry.read("session")).revision).toBe(0);
        }
    } finally { writer.close(); registry.close(); await rm(directory, { recursive: true, force: true }); }
});

test("real independent SQLite writer yields during admission ledger writes without losing original step identity", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-ledger-busy-"));
    const path = join(directory, "registry.db");
    const service = new ExecutionAdmissionService({ database: path, instance });
    const writer = new Database(path);
    try {
        const request = { requestId: "stable-request", snapshot: { sessionId: "session", control, blocked: false,
            candidates: [{ workId: "work", workRevision: 0, stage: "reasonReady", requiresRecovery: false }] } };
        const admitted = await releaseWriter(writer, () => service.admit(request));
        expect(admitted.status).toBe("admitted");
        expect(await service.admit(request)).toEqual(admitted);
        const paused = { ...control, revision: 1, controlGeneration: 1, status: "paused" as const };
        const observed = await releaseWriter(writer, () => service.observeControl("session", paused));
        expect(observed).toMatchObject({ status: "applied", receipt: { decision: { kind: "accepted" } } });
        expect(await service.observeControl("session", paused)).toEqual(observed);
        expect((await service.registry.read("session")).budgets[0]?.attempts).toBe(1);
    } finally { writer.close(); service.close(); await rm(directory, { recursive: true, force: true }); }
});

async function busyError(): Promise<SQLiteError> {
    const directory = await mkdtemp(join(tmpdir(), "peri-busy-error-"));
    const path = join(directory, "busy.db");
    const writer = new Database(path, { create: true });
    const contender = new Database(path);
    try {
        writer.exec("BEGIN IMMEDIATE");
        contender.exec("PRAGMA busy_timeout=0");
        try { contender.exec("BEGIN IMMEDIATE"); }
        catch (error) {
            if (error instanceof SQLiteError) return error;
            throw error;
        }
        throw new Error("Expected independent writer to cause SQLITE_BUSY");
    } finally { contender.close(); writer.close(); await rm(directory, { recursive: true, force: true }); }
}

test("typed busy detection excludes message-only busy and unrelated SQLite errors", async () => {
    const error = await busyError();
    expect(isSqliteBusy(error)).toBe(true);
    expect(isSqliteBusy(Object.assign(new Error("database is locked"), { code: "SQLITE_BUSY" }))).toBe(false);
    expect(isSqliteBusy(Object.assign(new Error("database is locked"), { errno: 6, code: "SQLITE_LOCKED" }))).toBe(false);
    const database = new Database(":memory:");
    try {
        try { database.exec("SELECT missing_column"); }
        catch (unrelated) {
            expect(unrelated).toBeInstanceOf(SQLiteError);
            expect(isSqliteBusy(unrelated)).toBe(false);
            return;
        }
        throw new Error("Expected invalid query to fail");
    } finally { database.close(); }
});

test("readonly busy retries yield before succeeding and preserve unrelated errors", async () => {
    const busy = await busyError();
    let calls = 0;
    let yielded = false;
    const heartbeat = new Promise<void>((resolve) => setTimeout(() => { yielded = true; resolve(); }, 0));
    expect(await retrySqliteBusy(() => {
        calls++;
        if (!yielded) throw busy;
        return "read-result";
    })).toBe("read-result");
    await heartbeat;
    expect(calls).toBeGreaterThan(1);
    const error = new Error("closed database");
    await expect(retrySqliteBusy(() => { throw error; })).rejects.toBe(error);
});

test("busy retry stops at its original deadline without starting another operation", async () => {
    const error = await busyError();
    const clock = spyOn(performance, "now").mockReturnValue(0);
    let attempts = 0;
    try {
        const pending = retrySqliteBusy(() => { attempts++; throw error; });
        clock.mockReturnValue(5000);
        await expect(pending).rejects.toBe(error);
        expect(attempts).toBe(1);
    } finally { clock.mockRestore(); }
});

test("registry freezes the original mutation while asynchronously waiting for a writer", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-stable-busy-"));
    const path = join(directory, "registry.db");
    const registry = new SqliteExecutionRegistry({ path });
    const writer = new Database(path);
    try {
        const mutable = structuredClone(command);
        const applied = await releaseWriter(writer, () => {
            const pending = registry.apply(mutable);
            mutable.mutationId = "changed-during-wait";
            mutable.expectedRevision = 100;
            return pending;
        });
        expect(applied).toMatchObject({ status: "applied", receipt: { decision: { kind: "accepted" } } });
        expect(await registry.resolve(command)).toEqual(applied);
        expect((await registry.read("session")).revision).toBe(1);
    } finally { writer.close(); registry.close(); await rm(directory, { recursive: true, force: true }); }
});

test.each(["callback", "commit"] as const)("ledger never retries SQLITE_BUSY after %s starts", async (phase) => {
    const error = await busyError();
    let attempts = 0;
    let callbacks = 0;
    const database = { transaction: (operation: () => unknown) => ({ immediate: () => {
        attempts++;
        operation();
        throw error;
    } }) } as unknown as Database;
    await expect(sqliteImmediate(database, () => {
        callbacks++;
        if (phase === "callback") throw error;
        return "effect";
    })).rejects.toBe(error);
    expect(attempts).toBe(1);
    expect(callbacks).toBe(1);
});

test.each(["callback", "commit"] as const)("registry preserves Unknown without replay after %s busy", async (phase) => {
    const error = await busyError();
    let attempts = 0;
    const registry = Object.assign(Object.create(SqliteExecutionRegistry.prototype), {
        database: {
            query: () => ({ get: () => {
                if (phase === "callback") throw error;
                return null;
            }, run: () => ({ changes: 1 }) }),
            transaction: (operation: () => unknown) => ({ immediate: () => {
                attempts++;
                operation();
                throw error;
            } }),
        },
    }) as SqliteExecutionRegistry;
    const diagnostic = spyOn(console, "error").mockImplementation(() => {});
    try {
        expect(await registry.apply(command)).toEqual({ status: "unknown" });
        expect(attempts).toBe(1);
        expect(diagnostic.mock.calls[0]?.[1]).toMatchObject({ phase: phase === "callback" ? "readMutation" : "commit", error });
    } finally { diagnostic.mockRestore(); }
});

test("closing a registry while it waits for a writer preserves failure instead of applying later", async () => {
    const directory = await mkdtemp(join(tmpdir(), "peri-close-busy-"));
    const path = join(directory, "registry.db");
    const registry = new SqliteExecutionRegistry({ path });
    const writer = new Database(path);
    const diagnostic = spyOn(console, "error").mockImplementation(() => {});
    try {
        writer.exec("BEGIN IMMEDIATE");
        const pending = registry.apply(command);
        registry.close();
        expect(await pending).toEqual({ status: "unknown" });
        writer.exec("COMMIT");
        expect(writer.query("SELECT session_id FROM sdk_execution_records").all()).toEqual([]);
    } finally { diagnostic.mockRestore(); writer.close(); await rm(directory, { recursive: true, force: true }); }
});
