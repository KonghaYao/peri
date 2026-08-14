type ClockLease = { token: symbol; now: number; released: boolean };
type ClockRegistry = { hostNow: typeof Date.now; leases: ClockLease[] };

const registryKey = Symbol.for('acp-hub.visual-fixture.clock-registry');
const fixtureGlobal = globalThis as typeof globalThis & { [registryKey]?: ClockRegistry };
const registry = fixtureGlobal[registryKey] ??= { hostNow: Date.now, leases: [] };

function applyClock(): void {
  const current = registry.leases.at(-1);
  Date.now = current ? () => current.now : registry.hostNow;
}

/**
 * Scope deterministic presentation time to a visual-fixture installation.
 * Leases may be released more than once or out of order, which keeps HMR and
 * nested test mounts from restoring the wrong clock.
 */
export function acquireFixtureClock(now: number): () => void {
  const lease: ClockLease = { token: Symbol('fixture-clock'), now, released: false };
  registry.leases.push(lease);
  applyClock();
  return () => {
    if (lease.released) return;
    lease.released = true;
    const index = registry.leases.findIndex((candidate) => candidate.token === lease.token);
    if (index >= 0) registry.leases.splice(index, 1);
    applyClock();
  };
}
