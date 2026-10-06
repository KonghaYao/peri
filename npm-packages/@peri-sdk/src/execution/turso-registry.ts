import { connect, type Connection, type Config, type Transaction } from "@tursodatabase/serverless";
import type { ExecutionMutation, ExecutionRecord, ExecutionRegistry, ExecutionResolution } from "./types";
import { decideExecution, emptyExecutionRecord, mutationDigest } from "./registry-model";
import {
    ExecutionMutationConflictError, REGISTRY_SCHEMA, READ_RECORD, READ_MUTATION, WRITE_RECORD, WRITE_MUTATION,
    sqlMutation, READ_PROTOCOL_VERSION, type SqlMutationRow,
} from "./registry-ledger";

export class TursoExecutionRegistry implements ExecutionRegistry {
    readonly durability = "durable" as const;
    private readonly connection: Connection;
    private readonly ready: Promise<unknown>;

    constructor(options: Config) {
        this.connection = connect(options);
        this.ready = this.connection.batch(REGISTRY_SCHEMA, "immediate").then(async () => {
            const metadata = await this.connection.get(READ_PROTOCOL_VERSION) as { protocol_version: number } | undefined;
            if (metadata?.protocol_version !== 1) throw new Error("Unsupported SDK execution registry protocol version");
        });
        void this.ready.catch(() => {});
    }

    async read(sessionId: string): Promise<ExecutionRecord> {
        const empty = emptyExecutionRecord(sessionId);
        await this.ready;
        const row = await this.connection.get(READ_RECORD, sessionId) as { payload: string } | undefined;
        return row ? JSON.parse(row.payload) as ExecutionRecord : empty;
    }

    private async mutate(command: ExecutionMutation, seal: boolean): Promise<ExecutionResolution> {
        const digest = mutationDigest(command);
        try {
            await this.ready;
            const operation = this.connection.transactionAsync(async (transaction: Transaction): Promise<ExecutionResolution> => {
                const existing = await transaction.get(READ_MUTATION, command.sessionId, command.mutationId) as SqlMutationRow | undefined;
                if (existing) return sqlMutation(command, digest, existing);
                if (seal) {
                    await transaction.run(WRITE_MUTATION, command.sessionId, command.mutationId, digest, "notApplied", null);
                    return { status: "notApplied" };
                }
                const row = await transaction.get(READ_RECORD, command.sessionId) as { payload: string } | undefined;
                const current = row ? JSON.parse(row.payload) as ExecutionRecord : emptyExecutionRecord(command.sessionId);
                const receipt = decideExecution(command, current);
                if (receipt.decision.kind === "accepted") await transaction.run(WRITE_RECORD, command.sessionId, JSON.stringify(receipt.state));
                await transaction.run(WRITE_MUTATION, command.sessionId, command.mutationId, digest, "applied", JSON.stringify(receipt));
                return { status: "applied", receipt };
            });
            return await operation.immediate() as ExecutionResolution;
        } catch (error) {
            if (error instanceof ExecutionMutationConflictError) throw error;
            return { status: "unknown" };
        }
    }

    apply(command: ExecutionMutation): Promise<ExecutionResolution> { return this.mutate(command, false); }
    resolve(command: ExecutionMutation): Promise<ExecutionResolution> { return this.mutate(command, true); }
    async close(): Promise<void> { await this.connection.close(); }
}
