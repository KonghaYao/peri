import type { AtomicManagedAgentKv } from "../kv/types";
import type { ExecutionMutation, ExecutionRecord, ExecutionRegistry, ExecutionResolution } from "./types";
import { decideExecution, emptyExecutionRecord, mutationDigest } from "./registry-model";
import { ExecutionMutationConflictError, replayMutation, type MutationEntry } from "./registry-ledger";

export type ExecutionKvValue = { protocolVersion: 1; version: number; record: ExecutionRecord; mutations: MutationEntry[] };
export interface AtomicExecutionKv extends AtomicManagedAgentKv {
    readonly durability: "durable" | "bestEffort";
    readExecutionValue(key: string): Promise<ExecutionKvValue | null>;
    compareAndSetExecutionValue(key: string, expectedVersion: number | null, next: ExecutionKvValue): Promise<boolean>;
}

export class KvExecutionRegistry implements ExecutionRegistry {
    readonly durability: "durable" | "bestEffort";

    constructor(private readonly kv: AtomicExecutionKv) {
        if (typeof kv.readExecutionValue !== "function" || typeof kv.compareAndSetExecutionValue !== "function")
            throw new TypeError("Execution KV requires atomic read and versioned CAS; claim/release is insufficient");
        this.durability = kv.durability;
    }

    private key(sessionId: string): string { return `peri:sdk-execution:session:${encodeURIComponent(sessionId)}`; }

    async read(sessionId: string): Promise<ExecutionRecord> {
        const empty = emptyExecutionRecord(sessionId);
        const value = await this.kv.readExecutionValue(this.key(sessionId));
        if (value && value.protocolVersion !== 1) throw new TypeError("Unsupported SDK execution registry protocol version");
        return structuredClone(value?.record ?? empty);
    }

    private async mutate(command: ExecutionMutation, seal: boolean): Promise<ExecutionResolution> {
        const digest = mutationDigest(command);
        const key = this.key(command.sessionId);
        try {
            for (let retries = 0; retries < 16; retries++) {
                const previous = await this.kv.readExecutionValue(key);
                if (previous && previous.protocolVersion !== 1) return { status: "unknown" };
                const existing = previous?.mutations.find((entry) => entry.mutationId === command.mutationId);
                if (existing) return replayMutation(command, digest, existing);
                const current = previous?.record ?? emptyExecutionRecord(command.sessionId);
                const result: ExecutionResolution = seal ? { status: "notApplied" }
                    : { status: "applied", receipt: decideExecution(command, current) };
                const state = result.status === "applied" && result.receipt.decision.kind === "accepted" ? result.receipt.state : current;
                const next: ExecutionKvValue = {
                    protocolVersion: 1,
                    version: (previous?.version ?? 0) + 1,
                    record: state,
                    mutations: [...(previous?.mutations ?? []), { mutationId: command.mutationId, digest, result }],
                };
                if (!Number.isSafeInteger(next.version)) return { status: "unknown" };
                if (await this.kv.compareAndSetExecutionValue(key, previous?.version ?? null, next)) return structuredClone(result);
            }
            return { status: "unknown" };
        } catch (error) {
            if (error instanceof ExecutionMutationConflictError) throw error;
            return { status: "unknown" };
        }
    }

    apply(command: ExecutionMutation): Promise<ExecutionResolution> { return this.mutate(command, false); }
    resolve(command: ExecutionMutation): Promise<ExecutionResolution> { return this.mutate(command, true); }
}
