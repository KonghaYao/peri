import type { AtomicManagedAgentKv } from "./types";
import { AgentClaimConflictError } from "./agent-claim-conflict-error";

export function claimKey(kind: "agent" | "session", workspaceId: string, id: string): string {
  return `peri:managed-agent:${encodeURIComponent(workspaceId)}:${kind}:${encodeURIComponent(id)}`;
}

/** A single Agent's claims are acquired in deterministic order and released by owner. */
export class AgentClaims {
  private readonly owner = crypto.randomUUID();
  private readonly keys: string[] = [];

  constructor(
    private readonly kv: AtomicManagedAgentKv,
    private readonly workspaceId: string,
    private readonly agentId: string,
  ) {}

  async claimAgent(): Promise<void> {
    await this.claim(claimKey("agent", this.workspaceId, this.agentId));
  }

  async claimSession(sessionId: string): Promise<void> {
    await this.claim(claimKey("session", this.workspaceId, sessionId));
  }

  private async claim(key: string): Promise<void> {
    if (!(await this.kv.claimIfAbsent(key, this.owner))) throw new AgentClaimConflictError(key);
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
