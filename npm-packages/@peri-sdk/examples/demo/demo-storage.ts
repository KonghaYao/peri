import type { SessionOwnershipStorage } from "../../src/chat/types";

/** Demo 进程内 storage：只用于单进程演示；多实例必须由宿主提供共享实现。 */
export class DemoOwnershipStorage implements SessionOwnershipStorage {
    private readonly values = new Map<string, unknown>();

    async getItem(key: string): Promise<unknown> {
        return this.values.get(key) ?? null;
    }

    async setItem(key: string, value: unknown): Promise<void> {
        this.values.set(key, structuredClone(value));
    }

    async removeItem(key: string): Promise<void> {
        this.values.delete(key);
    }
}
