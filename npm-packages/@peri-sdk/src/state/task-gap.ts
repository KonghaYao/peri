/**
 * 运行侧后台任务 revision 缺口检测（无 Yjs）。
 *
 * `peri/unstable_event` 的 `bg-task-*` 事件带单调 revision；缺一帧时 View 不能凭猜测补齐。
 * 这里发起一次 `session/bg-tasks` 快照，并把结果作为 `task-snapshot` 事实交给 View。
 * 快照请求在途时去重：同一时刻最多一个；失败后由后续 delta 再触发。
 */

type TaskEventVerdict = "accepted" | "duplicate" | "gap" | "ignored";

const DELTA_EVENTS = new Set([
    "bg-task-started", "bg-task-completed", "bg-task-cancelled", "bg-task-updated",
]);

function record(value: unknown): Record<string, unknown> | null {
    return value !== null && typeof value === "object" && !Array.isArray(value)
        ? value as Record<string, unknown>
        : null;
}

function finite(value: unknown): number | null {
    return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}

export class TaskGapDetector {
    private revision: number | null = null;
    private awaitingSnapshot = false;
    private inFlight = false;
    private request: (() => Promise<unknown>) | null = null;

    constructor(private readonly onSnapshot: (snapshot: unknown) => void) {}

    /** 绑定快照请求器；已有缺口时立即补一次。 */
    setRequester(request: (() => Promise<unknown>) | null): void {
        this.request = request;
        if (request && this.awaitingSnapshot) this.requestSnapshot();
    }

    /**
     * 观察一条后台任务 delta。返回 `gap` 表示 revision 不连续（或首次不是 1），
     * 调用方仍可把该 delta 交给 View；快照会随后覆盖。
     */
    observe(event: string | null, data: unknown): TaskEventVerdict {
        if (!event || !DELTA_EVENTS.has(event)) return "ignored";
        const value = record(data);
        if (!value) return "ignored";
        const revision = finite(value.revision);
        if (revision === null) return this.revision === null ? "accepted" : "ignored";
        if (this.awaitingSnapshot || this.inFlight) {
            this.awaitingSnapshot = true;
            this.requestSnapshot();
            return "gap";
        }
        if (this.revision === null) {
            if (revision === 1) {
                this.revision = revision;
                return "accepted";
            }
            return this.markGap();
        }
        if (revision === this.revision + 1) {
            this.revision = revision;
            return "accepted";
        }
        if (revision > this.revision + 1) return this.markGap();
        return "duplicate";
    }

    /** 接纳一份快照；revision 回退的旧快照不覆盖已观察到的进度。 */
    acceptSnapshot(snapshot: unknown): boolean {
        const value = record(snapshot);
        const revision = finite(value?.revision);
        if (revision === null || !Number.isInteger(revision) || !Array.isArray(value?.tasks)) return false;
        if (this.revision !== null && revision < this.revision) return false;
        this.revision = revision;
        this.awaitingSnapshot = false;
        return true;
    }

    /** 绑定结束/关闭：丢弃进度与在途标记，不再发起请求。 */
    reset(): void {
        this.revision = null;
        this.awaitingSnapshot = false;
        this.inFlight = false;
        this.request = null;
    }

    private markGap(): TaskEventVerdict {
        this.awaitingSnapshot = true;
        this.requestSnapshot();
        return "gap";
    }

    private requestSnapshot(): void {
        if (this.inFlight || !this.request) return;
        this.inFlight = true;
        let pending: Promise<unknown>;
        try {
            pending = this.request();
        } catch {
            this.inFlight = false;
            return;
        }
        void pending.then((snapshot) => {
            this.inFlight = false;
            if (this.acceptSnapshot(snapshot)) this.onSnapshot(snapshot);
        }, () => {
            // 后续 delta 再触发；上一 revision 仍不可信。
            this.inFlight = false;
        });
    }
}
