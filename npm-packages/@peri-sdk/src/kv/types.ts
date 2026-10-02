/** An unstorage-backed adapter must implement these operations atomically in its driver.
 * Plain unstorage Storage has no compare-and-set operation and is intentionally rejected.
 */
export interface AtomicManagedAgentKv {
  claimIfAbsent(key: string, owner: string): Promise<boolean>;
  releaseIfOwner(key: string, owner: string): Promise<boolean>;
}

export function requireAtomicKv(value: unknown): asserts value is AtomicManagedAgentKv {
  if (
    !value ||
    typeof value !== "object" ||
    typeof (value as Partial<AtomicManagedAgentKv>).claimIfAbsent !== "function" ||
    typeof (value as Partial<AtomicManagedAgentKv>).releaseIfOwner !== "function"
  ) {
    throw new TypeError("ManagedAgents requires an atomic claimIfAbsent/releaseIfOwner KV adapter");
  }
}
