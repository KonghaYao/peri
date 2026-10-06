import type { ExecutionMutation, ExecutionReceipt, ExecutionResolution } from "./types";

export type MutationEntry = { mutationId: string; digest: string; result: ExecutionResolution };

export class ExecutionMutationConflictError extends Error {
    constructor(readonly command: ExecutionMutation) {
        super("Execution mutationId cannot be reused with different parameters");
        this.name = "ExecutionMutationConflictError";
    }
}

export function replayMutation(command: ExecutionMutation, digest: string, entry: MutationEntry): ExecutionResolution {
    if (entry.digest !== digest) throw new ExecutionMutationConflictError(command);
    return structuredClone(entry.result);
}

export type SqlMutationRow = { digest: string; status: "applied" | "notApplied"; receipt: string | null };
export function sqlMutation(command: ExecutionMutation, digest: string, row: SqlMutationRow): ExecutionResolution {
    const result: ExecutionResolution = row.status === "applied"
        ? { status: "applied", receipt: JSON.parse(row.receipt!) as ExecutionReceipt }
        : { status: "notApplied" };
    return replayMutation(command, digest, { mutationId: command.mutationId, digest: row.digest, result });
}

export const REGISTRY_SCHEMA = [
    "CREATE TABLE IF NOT EXISTS sdk_execution_registry_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), protocol_version INTEGER NOT NULL)",
    "INSERT OR IGNORE INTO sdk_execution_registry_meta(singleton,protocol_version) VALUES (1,1)",
    "CREATE TABLE IF NOT EXISTS sdk_execution_records (session_id TEXT PRIMARY KEY, payload TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS sdk_execution_mutations (session_id TEXT NOT NULL, mutation_id TEXT NOT NULL, digest TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('applied','notApplied')), receipt TEXT, PRIMARY KEY(session_id, mutation_id))",
];
export const READ_PROTOCOL_VERSION = "SELECT protocol_version FROM sdk_execution_registry_meta WHERE singleton=1";
export const READ_RECORD = "SELECT payload FROM sdk_execution_records WHERE session_id = ?";
export const READ_MUTATION = "SELECT digest, status, receipt FROM sdk_execution_mutations WHERE session_id = ? AND mutation_id = ?";
export const WRITE_RECORD = "INSERT INTO sdk_execution_records(session_id, payload) VALUES (?, ?) ON CONFLICT(session_id) DO UPDATE SET payload = excluded.payload";
export const WRITE_MUTATION = "INSERT INTO sdk_execution_mutations(session_id, mutation_id, digest, status, receipt) VALUES (?, ?, ?, ?, ?)";
