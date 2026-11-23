import { ProtocolError } from '@modelcontextprotocol/client';

type Payload = Record<string, unknown>;
type ReadProvider = (method: string, params: Payload) => Promise<Payload>;

function record(value: unknown): value is Payload {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function taskFields(result: Payload): Payload {
  return typeof result.taskId === 'string' || !record(result.task) ? result : result.task;
}

function terminal(status: unknown): boolean {
  return status === 'completed' || status === 'failed' || status === 'cancelled';
}

const minProbeMs = 1000;

/** Run-local observations of provider-owned tasks. No tool output or task result is retained. */
export class TaskActivity {
  private readonly tasks = new Map<string, { params: Payload; due: number; interval: number; createdAt?: string }>();
  private uncertain: symbol | undefined;
  private probing = false;
  private nextProbe = 0;

  constructor(
    private readonly read: ReadProvider,
    private readonly settled: () => void,
    private readonly now: () => number = Date.now,
  ) {}

  /** A late host status reply belongs to the task that existed when its request began. */
  observeRequest(method: string, params: Payload): (result: Payload) => void {
    const id = params.taskId;
    const generation = typeof id === 'string' ? this.tasks.get(id) : undefined;
    return (result) => {
      if (method === 'tools/call' || (typeof id === 'string' && generation && this.tasks.get(id) === generation))
        this.observe(method, params, result);
    };
  }

  observe(method: string, params: Payload, result: Payload): void {
    const task = taskFields(result);
    const id = task.taskId;
    if (typeof id !== 'string' || !id) return;
    if (method === 'tools/call') {
      if (result.resultType !== 'task') return;
      if (!terminal(task.status)) {
        const meta: Payload = {};
        if (record(params._meta)) {
          for (const key of ['peri/taskOwner', 'peri/threadOwner']) {
            if (typeof params._meta[key] === 'string') meta[key] = params._meta[key];
          }
        }
        const interval =
          typeof task.pollIntervalMs === 'number' && Number.isSafeInteger(task.pollIntervalMs)
            ? Math.max(minProbeMs, task.pollIntervalMs)
            : minProbeMs;
        this.track(id, meta, interval, task.createdAt);
        return;
      }
    } else if (method === 'tasks/get' || method === 'tasks/cancel' || method === 'tasks/update') {
      if (id !== params.taskId) return;
    } else if (method !== 'notifications/tasks' && method !== 'notifications/tasks/status') return;
    const createdAt = this.tasks.get(id)?.createdAt;
    if (createdAt && typeof task.createdAt === 'string' && createdAt !== task.createdAt) return;
    if (terminal(task.status)) this.finish(id);
  }

  private track(id: string, meta: Payload, interval: number, createdAt: unknown): void {
    this.tasks.set(id, {
      params: Object.keys(meta).length ? { taskId: id, _meta: meta } : { taskId: id },
      due: this.now() + interval,
      interval,
      createdAt: typeof createdAt === 'string' ? createdAt : undefined,
    });
  }

  private finish(id: string): void {
    if (this.tasks.delete(id) && !this.tasks.size && !this.uncertain) this.settled();
  }

  /** A cancelled Bash request can spawn work before the bridge receives its receipt. */
  interruptedBash(): void {
    this.uncertain = Symbol();
  }

  /** Called by the idle sweep. Observations run outside the HTTP request and never block the sweep. */
  busy(): boolean {
    if (!this.uncertain && !this.tasks.size) return false;
    const now = this.now();
    if (!this.probing && now >= this.nextProbe) {
      this.probing = true;
      this.nextProbe = now + minProbeMs;
      void this.reconcile(now)
        .catch(() => {
          // Unavailability, timeouts and malformed observations cannot prove settlement.
        })
        .finally(() => {
          this.probing = false;
        });
    }
    return true;
  }

  private async reconcile(now: number): Promise<void> {
    const interrupted = this.uncertain;
    if (interrupted) {
      // The workspace's host-only query covers all owners when owner is omitted.
      const result = await this.read('tools/call', { name: 'shell_task_list', arguments: {} });
      const content = result.content;
      if (!result.isError && Array.isArray(content) && record(content[0]) && typeof content[0].text === 'string') {
        const list: unknown = JSON.parse(content[0].text);
        if (!record(list) || list.unknown !== false || !Array.isArray(list.tasks) || this.uncertain !== interrupted)
          return;
        if (!list.tasks.every((entry) => record(entry) && typeof entry.taskId === 'string' && entry.taskId.length > 0))
          return;
        // All active workspace tasks are now accounted for. Let their normal
        // owner-scoped polls proceed even if another owner's task runs for days.
        for (const entry of list.tasks) {
          if (this.tasks.has(entry.taskId)) continue;
          const meta: Payload = {};
          if (typeof entry.owner === 'string') meta['peri/taskOwner'] = entry.owner;
          if (typeof entry.threadOwner === 'string') meta['peri/threadOwner'] = entry.threadOwner;
          this.track(entry.taskId, meta, minProbeMs, entry.startedAt);
        }
        this.uncertain = undefined;
        if (!this.tasks.size) this.settled();
      }
      return;
    }
    for (const [id, task] of this.tasks) {
      if (task.due > now) continue;
      // Rotate even on failure so an unreachable task cannot starve the other owners.
      this.tasks.delete(id);
      this.tasks.set(id, task);
      task.due = now + task.interval;
      try {
        const result = await this.read('tasks/get', task.params);
        if (this.tasks.get(id) === task) this.observe('tasks/get', task.params, result);
      } catch (error) {
        // This workspace provider drops terminal records after its retention window.
        if (
          error instanceof ProtocolError &&
          error.code === -32602 &&
          /\bunknown task$/i.test(error.message) &&
          this.tasks.get(id) === task
        )
          this.finish(id);
      }
      return;
    }
  }
}
