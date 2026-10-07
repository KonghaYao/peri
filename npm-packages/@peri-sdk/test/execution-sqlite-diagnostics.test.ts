import { expect, spyOn, test } from "bun:test";
import { SqliteExecutionRegistry } from "../src/execution/sqlite-registry";
import type { ExecutionMutation } from "../src/execution/types";

test.each(["apply", "resolve"] as const)("SQLite %s preserves Unknown and reports transaction stage with original busy error", async (operation) => {
    const error = Object.assign(new Error("synthetic database is locked"), { code: "SQLITE_BUSY" });
    const registry = Object.assign(Object.create(SqliteExecutionRegistry.prototype), {
        database: { transaction: () => ({ immediate: () => { throw error; } }) },
    }) as SqliteExecutionRegistry;
    const command: ExecutionMutation = {
        sessionId: "isolated-session", mutationId: "isolated-mutation", expectedRevision: 0,
        action: { kind: "observeControl", control: {
            lifecycle: 1, revision: 0, controlGeneration: 0, status: "active", attempt: null,
        } },
    };
    const diagnostic = spyOn(console, "error").mockImplementation(() => {});
    try {
        expect(await registry[operation](command)).toEqual({ status: "unknown" });
        expect(diagnostic).toHaveBeenCalledTimes(1);
        expect(diagnostic.mock.calls[0]?.[1]).toMatchObject({
            operation, phase: "acquireWriteLock", sessionId: command.sessionId,
            mutationId: command.mutationId, action: "observeControl", error,
        });
        expect(diagnostic.mock.calls[0]?.[1].error).toBe(error);
    } finally { diagnostic.mockRestore(); }
});
