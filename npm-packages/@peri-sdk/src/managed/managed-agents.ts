import { Agent } from "../agent/agent";
import type { AgentOptions } from "../agent/types";
import { requireAtomicKv, type AtomicManagedAgentKv } from "../kv/types";
import { AgentBusyError } from "./agent-busy-error";
import type { CloseOptions } from "../agent/session-close";

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

  /** Synchronous declaration. No claim or transport starts here. */
  createAgent(options: AgentOptions): Agent {
    if (!options.id) throw new TypeError("Agent id is required");
    if (this.agents.has(options.id)) throw new AgentBusyError(options.id);
    const agent = new Agent(options, this.kv);
    this.agents.set(options.id, agent);
    return agent;
  }

  async closeAgent(id: string, options?: CloseOptions): Promise<void> {
    const agent = this.agents.get(id);
    if (!agent) return;
    await agent.close(options);
    this.agents.delete(id);
  }

  async cleanupStartupAgent(id: string): Promise<void> {
    const agent = this.agents.get(id);
    if (!agent) return;
    await agent.session.cleanupStartup();
    this.agents.delete(id);
  }

  /** 关闭全部 Agent；任一失败都会聚合上报，不吞掉失败。 */
  async closeAll(options?: CloseOptions): Promise<void> {
    const results = await Promise.allSettled(
      [...this.agents.keys()].map((id) => this.closeAgent(id, options)),
    );
    const failures = results.filter((result): result is PromiseRejectedResult => result.status === "rejected");
    if (failures.length) throw new AggregateError(failures.map((result) => result.reason), "Agent closure is incomplete");
  }
}
