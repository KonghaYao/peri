export interface ReplayCallContext {
  owner: string;
  execution: string;
  call: string;
  attempt: number;
  purpose: string;
  parent?: { owner: string; execution: string; call: string; tool_call: string; slot?: string };
}

interface IdentityRun {
  bindIdentity(binding: { field: "owner" | "execution" | "call"; live: string; canonical: string; owner?: string; execution?: string; anchor: string }): void;
}

/**
 * 仅用于这些场景中已知根会话的一次用户输入；不会按全局 HTTP 到达顺序命名会话。
 * 每个 owner 同时只有一个 armed 动作，旧 execution 的映射不会在取消后复用。
 */
export class ScenarioIdentity {
  private readonly owners = new Map<string, string>();
  private readonly ownerAnchors = new Map<string, string>();
  private readonly executions = new Map<string, string>();
  private readonly armed = new Map<string, { action: string; purpose: string; call: string }>();
  private readonly queued = new Map<string, Array<{ action: string; purpose: string; call: string }>>();
  private readonly active = new Set<string>();
  readonly seen: ReplayCallContext[] = [];

  constructor(owners: ReadonlyArray<{ live: string; canonical: string }>) {
    for (const owner of owners) {
      this.registerOwner(owner.live, owner.canonical);
    }
  }

  registerOwner(live: string, canonical: string, anchor = "ACP session/new result"): void {
    if (!live || !canonical || !anchor || this.owners.has(live) || [...this.owners.values()].includes(canonical)) throw new Error("duplicate or invalid scenario owner");
    this.owners.set(live, canonical);
    this.ownerAnchors.set(live, anchor);
  }

  ownerBindings(): Array<{ live: string; canonical: string; anchor: string }> {
    return [...this.owners].map(([live, canonical]) => ({ live, canonical, anchor: this.ownerAnchors.get(live)! }));
  }

  register(live: string, canonical: string, anchor: string): void { this.registerOwner(live, canonical, anchor); }

  arm(owner: string, action: string, options: { purpose?: string; call?: string } = {}): void {
    if (!this.owners.has(owner) || this.armed.has(owner) || this.active.has(owner)) throw new Error(`owner cannot arm action: ${owner}`);
    this.armed.set(owner, { action, purpose: options.purpose ?? "reason", call: options.call ?? "reason:1" });
  }

  /** Only for explicitly submitted FIFO user inputs on one known owner, never global arrival order. */
  queue(owner: string, action: string, options: { purpose?: string; call?: string } = {}): void {
    if (!this.owners.has(owner) || (!this.active.has(owner) && !this.armed.has(owner))) throw new Error(`queue requires an existing owner action: ${owner}`);
    const queue = this.queued.get(owner) ?? [];
    queue.push({ action, purpose: options.purpose ?? "reason", call: options.call ?? "reason:1" });
    this.queued.set(owner, queue);
  }

  finish(owner: string): void {
    if (this.armed.has(owner)) throw new Error(`action finished without a model request: ${owner}`);
    if (this.queued.get(owner)?.length) throw new Error(`queued actions finished without model requests: ${owner}`);
    if (!this.active.delete(owner)) throw new Error(`no active action: ${owner}`);
  }

  resolve = async (context: ReplayCallContext, run: IdentityRun): Promise<void> => {
    const owner = this.owners.get(context.owner);
    if (!owner) throw new Error(`unknown root owner in this scenario: ${context.owner}`);
    run.bindIdentity({ field: "owner", live: context.owner, canonical: owner, anchor: this.ownerAnchors.get(context.owner)! });
    const key = JSON.stringify([context.owner, context.execution]);
    let execution = this.executions.get(key);
    if (!execution) {
      const armed = this.armed.get(context.owner) ?? this.queued.get(context.owner)?.[0];
      if (!armed || context.purpose !== armed.purpose || context.call !== armed.call) {
        throw new Error(`unbound execution without unique armed root action: ${JSON.stringify(context)}`);
      }
      execution = armed.action;
      this.executions.set(key, execution);
      if (!this.armed.delete(context.owner)) this.queued.get(context.owner)!.shift();
      this.active.add(context.owner);
    }
    run.bindIdentity({ field: "execution", owner, live: context.execution, canonical: execution, anchor: `explicit user input ${execution}` });
    this.seen.push(structuredClone(context));
  };
}
