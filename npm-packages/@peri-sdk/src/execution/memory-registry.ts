import { MemoryKV } from "../kv/memory-kv";
import { KvExecutionRegistry, type AtomicExecutionKv, type ExecutionKvValue } from "./kv-registry";

export class MemoryExecutionKv implements AtomicExecutionKv {
    readonly durability = "bestEffort" as const;
    private readonly claims = new MemoryKV();
    private readonly values = new Map<string, ExecutionKvValue>();

    claimIfAbsent(key: string, owner: string): Promise<boolean> { return this.claims.claimIfAbsent(key, owner); }
    releaseIfOwner(key: string, owner: string): Promise<boolean> { return this.claims.releaseIfOwner(key, owner); }
    async readExecutionValue(key: string): Promise<ExecutionKvValue | null> {
        return structuredClone(this.values.get(key) ?? null);
    }
    async compareAndSetExecutionValue(key: string, expectedVersion: number | null, next: ExecutionKvValue): Promise<boolean> {
        if ((this.values.get(key)?.version ?? null) !== expectedVersion) return false;
        this.values.set(key, structuredClone(next));
        return true;
    }
}

export class MemoryExecutionRegistry extends KvExecutionRegistry {
    constructor(kv = new MemoryExecutionKv()) { super(kv); }
}
