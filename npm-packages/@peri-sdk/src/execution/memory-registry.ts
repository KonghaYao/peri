import type { AtomicExecutionKv, ExecutionKvValue } from "./kv-registry";

export class MemoryExecutionKv implements AtomicExecutionKv {
    readonly durability = "bestEffort" as const;
    private readonly values = new Map<string, ExecutionKvValue>();

    async readExecutionValue(key: string): Promise<ExecutionKvValue | null> {
        return structuredClone(this.values.get(key) ?? null);
    }
    async compareAndSetExecutionValue(key: string, expectedVersion: number | null, next: ExecutionKvValue): Promise<boolean> {
        if ((this.values.get(key)?.version ?? null) !== expectedVersion) return false;
        this.values.set(key, structuredClone(next));
        return true;
    }
}
