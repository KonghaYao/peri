import type { AtomicManagedAgentKv } from "./types";
import { SessionClaimConflictError } from "./session-claim-conflict-error";
import { sessionClaimKey } from "./session-claim-key";

export { sessionClaimKey } from "./session-claim-key";

export function agentClaimKey(sandboxId: string, agentId: string): string {
  return `peri:managed-agent:${encodeURIComponent(sandboxId)}:agent:${encodeURIComponent(agentId)}`;
}

/** A single Agent's claims are acquired in deterministic order and released by owner. */
export class AgentClaims {
  private readonly owner = crypto.randomUUID();
  private readonly keys: string[] = [];

  constructor(
    private readonly kv: AtomicManagedAgentKv,
    private readonly sandboxId: string,
    private readonly agentId: string,
  ) {}

  async claimAgent(): Promise<void> {
    await this.claim(agentClaimKey(this.sandboxId, this.agentId));
  }

  async claimSession(sessionId: string): Promise<void> {
    await this.claim(sessionClaimKey(sessionId));
  }

  private async claim(key: string): Promise<void> {
    if (!(await this.kv.claimIfAbsent(key, this.owner))) throw new SessionClaimConflictError(key);
    this.keys.push(key);
  }

  async release(): Promise<void> {
    while (this.keys.length > 0) {
      const key = this.keys.at(-1)!;
      if (!(await this.kv.releaseIfOwner(key, this.owner))) {
        throw new Error(`Cannot release claim owned by another instance: ${key}`);
      }
      this.keys.pop();
    }
  }
}
