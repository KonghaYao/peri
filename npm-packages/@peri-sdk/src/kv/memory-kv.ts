import type { AtomicManagedAgentKv } from "./types";

/** Atomic claims within one process. Claims are lost when the process exits. */
export class MemoryKV implements AtomicManagedAgentKv {
  private readonly owners = new Map<string, string>();

  async claimIfAbsent(key: string, owner: string): Promise<boolean> {
    if (this.owners.has(key)) return false;
    this.owners.set(key, owner);
    return true;
  }

  async releaseIfOwner(key: string, owner: string): Promise<boolean> {
    if (this.owners.get(key) !== owner) return false;
    this.owners.delete(key);
    return true;
  }
}
