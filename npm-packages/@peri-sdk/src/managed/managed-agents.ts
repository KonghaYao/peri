import { Agent } from "../agent/agent";
import type { AgentOptions } from "../agent/types";
import { requireAtomicKv, type AtomicManagedAgentKv } from "../kv/types";
import { AgentBusyError } from "./agent-busy-error";

export interface ManagedAgentsOptions {
  /** Must be an atomic adapter over the shared unjs KV driver, not plain Storage. */
  kv: AtomicManagedAgentKv;
}

export class ManagedAgents {
  private readonly agents = new Map<string, Agent>();
  private readonly kv: AtomicManagedAgentKv;

  constructor(options: ManagedAgentsOptions) {
    requireAtomicKv(options.kv);
    this.kv = options.kv;
  }

  /** Synchronous declaration. No claim, transport, or Peri process starts here. */
  createAgent(options: AgentOptions): Agent {
    if (!options.id) throw new TypeError("Agent id is required");
    if (this.agents.has(options.id)) throw new AgentBusyError(options.id);
    const agent = new Agent(options, this.kv);
    this.agents.set(options.id, agent);
    return agent;
  }

  async closeAgent(id: string): Promise<void> {
    const agent = this.agents.get(id);
    if (!agent) return;
    await agent.close();
    this.agents.delete(id);
  }

  async closeAll(): Promise<void> {
    await Promise.allSettled([...this.agents.keys()].map((id) => this.closeAgent(id)));
  }
}
