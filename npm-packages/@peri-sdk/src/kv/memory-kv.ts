import type { AtomicSessionOwnershipKv } from "./types";

/** 单进程原子 ownership；进程退出后归属丢失，不能当作持久所有权。 */
export class MemoryKV implements AtomicSessionOwnershipKv {
  private readonly owners = new Map<string, string>();

  async acquireIfAbsent(key: string, owner: string): Promise<boolean> {
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
