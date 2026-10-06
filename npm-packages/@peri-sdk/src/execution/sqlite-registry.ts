import { Database } from "bun:sqlite";
import type { ExecutionMutation, ExecutionRecord, ExecutionRegistry, ExecutionResolution, InstanceDescriptor } from "./types";
import { decideExecution, emptyExecutionRecord, mutationDigest } from "./registry-model";
import {
    ExecutionMutationConflictError, REGISTRY_SCHEMA, READ_RECORD, READ_MUTATION, WRITE_RECORD, WRITE_MUTATION,
    sqlMutation, READ_PROTOCOL_VERSION, type SqlMutationRow,
} from "./registry-ledger";

export class SqliteExecutionRegistry implements ExecutionRegistry {
    readonly durability = "durable" as const;
    private readonly database: Database;

    constructor(options: { path: string }) {
        if (options.path === ":memory:" || options.path === "") throw new TypeError("Durable execution registry requires a file path");
        this.database = new Database(options.path, { create: true });
        this.database.exec("PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL");
        for (const statement of REGISTRY_SCHEMA) this.database.exec(statement);
        if (this.database.query<{ protocol_version: number }, []>(READ_PROTOCOL_VERSION).get()?.protocol_version !== 1) {
            this.database.close();
            throw new Error("Unsupported SDK execution registry protocol version");
        }
    }

    async read(sessionId: string): Promise<ExecutionRecord> {
        const empty = emptyExecutionRecord(sessionId);
        const row = this.database.query<{ payload: string }, [string]>(READ_RECORD).get(sessionId);
        return row ? JSON.parse(row.payload) as ExecutionRecord : empty;
    }

    private mutate(command: ExecutionMutation, seal: boolean): ExecutionResolution {
        const digest = mutationDigest(command);
        try {
            return this.database.transaction((): ExecutionResolution => {
                const existing = this.database.query<SqlMutationRow, [string, string]>(READ_MUTATION).get(command.sessionId, command.mutationId);
                if (existing) return sqlMutation(command, digest, existing);
                if (seal) {
                    this.database.query(WRITE_MUTATION).run(command.sessionId, command.mutationId, digest, "notApplied", null);
                    return { status: "notApplied" };
                }
                const row = this.database.query<{ payload: string }, [string]>(READ_RECORD).get(command.sessionId);
                const current = row ? JSON.parse(row.payload) as ExecutionRecord : emptyExecutionRecord(command.sessionId);
                const receipt = decideExecution(command, current);
                if (receipt.decision.kind === "accepted")
                    this.database.query(WRITE_RECORD).run(command.sessionId, JSON.stringify(receipt.state));
                this.database.query(WRITE_MUTATION).run(command.sessionId, command.mutationId, digest, "applied", JSON.stringify(receipt));
                return { status: "applied", receipt };
            }).immediate();
        } catch (error) {
            if (error instanceof ExecutionMutationConflictError) throw error;
            return { status: "unknown" };
        }
    }

    async apply(command: ExecutionMutation): Promise<ExecutionResolution> { return this.mutate(command, false); }
    async resolve(command: ExecutionMutation): Promise<ExecutionResolution> { return this.mutate(command, true); }
    async instanceSessions(instance: InstanceDescriptor): Promise<string[]> {
        return this.database.query<{ session_id: string }, [string, string]>(
            "SELECT session_id FROM sdk_execution_records WHERE json_extract(payload,'$.instance.instanceId')=? AND json_extract(payload,'$.instance.generationId')=?",
        ).all(instance.instanceId, instance.generationId).map((row) => row.session_id);
    }
    close(): void { this.database.close(); }
}
