import { MemoryKV } from "../kv/memory-kv";
import type { AtomicExecutionKv, ExecutionKvValue } from "./kv-registry";

export class MemoryExecutionKv implements AtomicExecutionKv {
    readonly durability = "bestEffort" as const;
    private readonly ownership = new MemoryKV();
    private readonly values = new Map<string, ExecutionKvValue>();

    acquireIfAbsent(key: string, owner: string): Promise<boolean> { return this.ownership.acquireIfAbsent(key, owner); }
    releaseIfOwner(key: string, owner: string): Promise<boolean> { return this.ownership.releaseIfOwner(key, owner); }
    async readExecutionValue(key: string): Promise<ExecutionKvValue | null> {
        return structuredClone(this.values.get(key) ?? null);
    }
    async compareAndSetExecutionValue(key: string, expectedVersion: number | null, next: ExecutionKvValue): Promise<boolean> {
        if ((this.values.get(key)?.version ?? null) !== expectedVersion) return false;
        this.values.set(key, structuredClone(next));
        return true;
    }
}
