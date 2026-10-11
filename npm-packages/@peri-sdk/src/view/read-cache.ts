import * as Y from "yjs";

type Shared = Y.AbstractType<any>;

function immutable<T>(value: T): T {
    if (value && typeof value === "object" && !Object.isFrozen(value)) {
        for (const child of Object.values(value)) immutable(child);
        Object.freeze(value);
    }
    return value;
}

/** Cache follows Yjs ownership plus the tool reference edges of the read model. */
export class ReadCache {
    private readonly values = new WeakMap<Shared, unknown>();
    private readonly dependents = new WeakMap<Shared, Set<Shared>>();

    read<T>(type: Shared | null, build: () => T): T {
        if (!type) return build();
        if (this.values.has(type)) return this.values.get(type) as T;
        const value = immutable(build());
        this.values.set(type, value);
        return value;
    }

    depend(source: Shared | null, target: Shared): void {
        if (!source) return;
        let targets = this.dependents.get(source);
        if (!targets) { targets = new Set(); this.dependents.set(source, targets); }
        targets.add(target);
    }

    invalidate(transaction: Y.Transaction): void {
        const visited = new Set<Shared>();
        const invalidate = (type: Shared | null): void => {
            if (!type || visited.has(type)) return;
            visited.add(type);
            this.values.delete(type);
            const dependents = this.dependents.get(type);
            if (dependents) {
                this.dependents.delete(type);
                for (const dependent of dependents) invalidate(dependent);
            }
            invalidate(type.parent);
        };
        for (const [type] of transaction.changed) {
            invalidate(type);
            // A replaced/deleted tool still has dependents in cached entry blocks.
            for (const event of transaction.changedParentTypes.get(type) ?? []) {
                if (event.target !== type || !(event instanceof Y.YMapEvent)) continue;
                for (const { oldValue } of event.changes.keys.values()) {
                    if (oldValue instanceof Y.AbstractType) invalidate(oldValue);
                }
            }
        }
    }
}
