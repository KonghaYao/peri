import * as Y from "yjs";
import { DocModel } from "./doc-model";
import { finite, object, string, type RecordValue } from "./protocol";

const TERMINAL = new Set(["completed", "failed", "cancelled"]);
const MAX_TASKS = 200;
function safeSummary(raw: unknown): string | null {
    if (typeof raw !== "string") return null;
    const text = raw.trim()
        .replace(/\b(?:https?|wss?):\/\/[^\s<>'"`]+/giu, "[REDACTED_URL]")
        .replace(/\b(?:bearer\s+)?[A-Za-z0-9_-]*(?:token|secret|password|api[_-]?key)[A-Za-z0-9_-]*\s*[:=]\s*[^\s,;]+/giu, "[REDACTED_SECRET]")
        .replace(/\bBearer\s+[A-Za-z0-9._~+/-]+=*/giu, "[REDACTED_SECRET]")
        .replace(/(?:^|\s)(?:~\/|\/(?:Users|home|var|tmp|private|etc|opt|srv|workspace)\/)[^\s<>'"`]+/gu,
            (match) => `${match.startsWith(" ") ? " " : ""}[REDACTED_PATH]`);
    return text ? text.slice(0, 500) : null;
}
function validTime(raw: unknown): string | null {
    const value = string(raw);
    return value && Number.isFinite(Date.parse(value)) ? value : null;
}

/** Subagent and background task projections, with snapshot repair for revision gaps. */
export class TaskProjection {
    private backgroundRevision: number | null = null;
    private awaitingSnapshot = false;
    private requestSnapshot: (() => void) | null = null;

    constructor(private readonly docs: DocModel) {}

    setSnapshotRequester(request: (() => void) | null): void {
        this.requestSnapshot = request;
        if (request && this.awaitingSnapshot) request();
    }

    private needSnapshot(): void {
        this.awaitingSnapshot = true;
        this.requestSnapshot?.();
    }

    private upsert(id: string, kind: string, title: string, status: string, fields: RecordValue = {}): void {
        const tasks = this.docs.tasks();
        const task = tasks.get(id) ?? new Y.Map<unknown>();
        if (!tasks.has(id)) {
            task.set("taskId", id);
            task.set("kind", kind);
            task.set("title", title);
            tasks.set(id, task);
            (this.docs.sessionRoot().get("taskOrder") as Y.Array<string>).push([id]);
        }
        if (kind === "background" && (task.get("title") === "Background task" || task.get("title") === task.get("taskSubtype"))) {
            task.set("title", title);
        }
        task.set("status", status);
        for (const [key, value] of Object.entries(fields)) {
            if (value !== undefined) task.set(key, value);
        }
        const order = this.docs.sessionRoot().get("taskOrder") as Y.Array<string>;
        while (order.length > MAX_TASKS) {
            const ids = order.toArray();
            const terminal = ids.findIndex((taskId) => TERMINAL.has(String(tasks.get(taskId)?.get("status"))));
            const index = terminal < 0 ? 0 : terminal;
            tasks.delete(ids[index]!);
            order.delete(index, 1);
        }
    }

    acceptSubagent(type: string, value: RecordValue | null): boolean {
        if (!value || (type !== "subagent_started" && type !== "subagent_stopped")) return false;
        const id = string(value.instance_id);
        if (!id) return false;
        const existing = this.docs.tasks().get(id);
        if (existing && (existing.get("kind") !== "subagent" || TERMINAL.has(String(existing.get("status"))))) return false;
        this.docs.session.transact(() => this.upsert(
            id, "subagent", safeSummary(value.agent_name)?.slice(0, 120) ?? "Subagent",
            type === "subagent_started" ? "running" : value.is_error === true ? "failed" : "completed",
            type === "subagent_started"
                ? { isBackground: value.is_background === true, startedAt: new Date().toISOString() }
                : { completedAt: new Date().toISOString() },
        ));
        return true;
    }

    acceptBackground(name: string | null, data: RecordValue | null): boolean {
        if (name === "bg-task-snapshot") return this.acceptSnapshot(data);
        if (!data || !name || !["bg-task-started", "bg-task-completed", "bg-task-cancelled", "bg-task-updated"].includes(name)) return false;
        const id = string(data.task_id);
        if (!id || !this.acceptRevision(data.revision)) return false;
        const existing = this.docs.tasks().get(id);
        const status = name === "bg-task-started" ? "running"
            : name === "bg-task-cancelled" ? "cancelled"
            : name === "bg-task-completed" ? data.success === false ? "failed" : "completed"
            : string(data.status);
        if (!status) return false;
        if (name === "bg-task-updated" && !existing) {
            this.needSnapshot();
            return false;
        }
        if (existing && existing.get("kind") === "subagent") {
            const revision = finite(data.revision);
            if (revision !== null) this.backgroundRevision = revision;
            return false;
        }
        if (existing && TERMINAL.has(String(existing.get("status")))) {
            const revision = finite(data.revision);
            if (revision !== null) this.backgroundRevision = revision;
            return false;
        }
        const text = safeSummary(data.summary);
        this.docs.session.transact(() => this.upsert(id, "background", text?.slice(0, 120) ?? string(data.kind) ?? "Background task", status, {
            isBackground: true,
            ...(string(data.kind) ? { taskSubtype: data.kind } : {}),
            ...(text ? { summary: text } : {}),
            ...(validTime(data.started_at) ? { startedAt: validTime(data.started_at) } : {}),
            ...(finite(data.duration_ms) !== null ? { durationMs: finite(data.duration_ms) } : {}),
            ...(TERMINAL.has(status) ? { completedAt: new Date().toISOString() } : {}),
            updatedAt: new Date().toISOString(),
        }));
        const revision = finite(data.revision);
        if (revision !== null) this.backgroundRevision = revision;
        return true;
    }

    private acceptRevision(raw: unknown): boolean {
        const revision = finite(raw);
        if (revision === null) return this.backgroundRevision === null;
        if (this.awaitingSnapshot) { this.requestSnapshot?.(); return false; }
        if (this.backgroundRevision === null) {
            if (revision === 1) return true;
            this.needSnapshot();
            return false;
        }
        if (revision === this.backgroundRevision + 1) return true;
        if (revision > this.backgroundRevision + 1) this.needSnapshot();
        return false;
    }

    private acceptSnapshot(data: RecordValue | null): boolean {
        const revision = finite(data?.revision);
        if (revision === null || !Number.isInteger(revision) || !Array.isArray(data?.tasks)) return false;
        if (this.backgroundRevision !== null && revision < this.backgroundRevision) return false;
        const rows = data.tasks.map((raw) => object(raw));
        if (rows.some((row) => !row || !string(row.task_id) || !string(row.status))) return false;
        this.docs.session.transact(() => {
            const tasks = this.docs.tasks();
            const order = this.docs.sessionRoot().get("taskOrder") as Y.Array<string>;
            const incoming = new Set(rows.map((row) => string(row?.task_id)!));
            for (const [id, task] of tasks) {
                if (task.get("kind") === "background" && !incoming.has(id)) tasks.delete(id);
            }
            const kept = order.toArray().filter((id) => tasks.has(id));
            if (kept.length !== order.length) {
                order.delete(0, order.length);
                if (kept.length) order.push(kept);
            }
            for (const row of rows) {
                const id = string(row?.task_id)!;
                if (tasks.get(id)?.get("kind") === "subagent") continue;
                const text = safeSummary(row?.summary);
                const status = string(row?.status)!;
                this.upsert(id, "background", text?.slice(0, 120) ?? string(row?.kind) ?? "Background task", status, {
                    isBackground: true,
                    ...(string(row?.kind) ? { taskSubtype: row?.kind } : {}),
                    ...(text ? { summary: text } : {}),
                    ...(validTime(row?.started_at) ? { startedAt: validTime(row?.started_at) } : {}),
                    ...(finite(row?.duration_ms) !== null ? { durationMs: finite(row?.duration_ms) } : {}),
                    ...(TERMINAL.has(status) ? { completedAt: new Date().toISOString() } : {}),
                    updatedAt: new Date().toISOString(),
                });
            }
        });
        this.backgroundRevision = revision;
        this.awaitingSnapshot = false;
        return true;
    }
}
