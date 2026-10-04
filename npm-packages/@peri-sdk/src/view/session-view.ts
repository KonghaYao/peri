import * as Y from "yjs";

export type ToolView = {
    toolCallId: string;
    turnId: string | null;
    name: string;
    status: string;
    kind?: string;
    arguments?: unknown;
    result?: unknown;
};
export type EntryBlockView =
    | { blockId: string; type: "text" | "reasoning"; text: string; visibility?: string }
    | { blockId: string; type: "tool_call"; toolCallId: string; tool: ToolView | null };
export type EntryView = {
    entryId: string;
    turnId: string;
    role: "user" | "assistant";
    status: string;
    messageId?: string;
    tokenUsage?: Record<string, unknown>;
    blocks: EntryBlockView[];
};
export type PlanEntryView = { content: string; status: string; activeForm?: string };
export type TaskView = {
    taskId: string;
    kind: string;
    title: string;
    status: string;
    summary?: string;
    isBackground?: boolean;
    taskSubtype?: string;
    startedAt?: string;
    completedAt?: string;
    updatedAt?: string;
    durationMs?: number;
};
export type InteractionView = {
    id: number;
    kind: "permission" | "elicitation";
    sessionId: string;
    toolCallId?: string;
    title?: string;
    message?: string;
    options?: Array<{ optionId: string; name: string }>;
    fields?: Array<{ key: string; title: string; description: string }>;
};
export type InputQueueView = {
    generation: string;
    revision: number;
    activeRequestId?: string;
    items: Array<{ inputId: string; state: string }>;
};
export type SessionInfoView = {
    title?: string;
    model?: string;
    mode?: string;
    updatedAt?: string;
    activeRequestId?: string;
    activeTurnInteractionStatus?: string;
    compactionStatus?: string;
    suspendedTurnId?: string;
    rewindRevision?: number;
    modeState?: Record<string, unknown>;
    modelState?: Record<string, unknown>;
    goal?: Record<string, unknown>;
    stateSnapshotMeta?: Record<string, unknown>;
    compaction?: Record<string, unknown>;
    configOptions?: Array<Record<string, unknown>>;
    availableCommands?: Array<Record<string, unknown>>;
};
export type SessionView = {
    schemaVersion: number;
    entries: EntryView[];
    activeTurnId: string | null;
    activeTurnStatus: string | null;
    session: SessionInfoView;
    tasks: TaskView[];
    plan: PlanEntryView[];
    plansByTurn: Record<string, PlanEntryView[]>;
    pendingInteractions: InteractionView[];
    inputQueue: InputQueueView | null;
};

function map(value: unknown): Y.Map<unknown> | null { return value instanceof Y.Map ? value : null; }
function root(doc: Y.Doc): Y.Map<unknown> | null {
    // applyUpdate leaves a generic root placeholder until getMap resolves its type.
    return doc.share.has("root") ? doc.getMap<unknown>("root") : null;
}
function array(value: unknown): Y.Array<unknown> | null { return value instanceof Y.Array ? value : null; }
function str(value: unknown): string | null { return typeof value === "string" ? value : null; }
function num(value: unknown): number | null { return typeof value === "number" && Number.isFinite(value) ? value : null; }
function obj(value: unknown): Record<string, unknown> | null {
    return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}
function copy(value: unknown): unknown {
    if (value instanceof Y.AbstractType) return copy(value.toJSON());
    if (Array.isArray(value)) return value.map(copy);
    if (obj(value)) return Object.fromEntries(Object.entries(value as Record<string, unknown>).map(([key, part]) => [key, copy(part)]));
    return value;
}
function record(value: unknown): Record<string, unknown> | null { return obj(copy(value)); }
function list<T>(value: unknown, read: (item: unknown) => T | null): T[] {
    const values = array(value)?.toArray() ?? (Array.isArray(value) ? value : []);
    return values.flatMap((item) => { const parsed = read(item); return parsed === null ? [] : [parsed]; });
}

function toolView(value: unknown): ToolView | null {
    const tool = map(value);
    const toolCallId = str(tool?.get("toolCallId"));
    if (!tool || !toolCallId) return null;
    const result: ToolView = {
        toolCallId, turnId: str(tool.get("turnId")), name: str(tool.get("name")) ?? "Tool",
        status: str(tool.get("status")) ?? "pending",
    };
    const kind = str(tool.get("kind"));
    if (kind) result.kind = kind;
    if (tool.has("arguments")) result.arguments = copy(tool.get("arguments"));
    if (tool.has("result")) result.result = copy(tool.get("result"));
    return result;
}

function blockView(value: unknown, tools: Y.Map<unknown> | null): EntryBlockView | null {
    const block = map(value);
    const blockId = str(block?.get("blockId"));
    const type = str(block?.get("type"));
    if (!block || !blockId) return null;
    if (type === "text" || type === "reasoning") {
        const text = block.get("text");
        const result: EntryBlockView = { blockId, type, text: text instanceof Y.Text ? text.toString() : str(text) ?? "" };
        const visibility = str(block.get("visibility"));
        if (visibility && type === "reasoning") result.visibility = visibility;
        return result;
    }
    if (type === "tool_call") {
        const toolCallId = str(block.get("toolCallId"));
        if (toolCallId) return { blockId, type, toolCallId, tool: toolView(tools?.get(toolCallId)) };
    }
    return null;
}

function entryView(value: unknown, tools: Y.Map<unknown> | null): EntryView | null {
    const entry = map(value);
    const entryId = str(entry?.get("entryId"));
    const turnId = str(entry?.get("turnId"));
    const role = str(entry?.get("role"));
    if (!entry || !entryId || !turnId || (role !== "user" && role !== "assistant")) return null;
    const blocks = map(entry.get("blocks"));
    const result: EntryView = {
        entryId, turnId, role, status: str(entry.get("status")) ?? "pending",
        blocks: list(entry.get("blockOrder"), (id) => blockView(blocks?.get(String(id)), tools)),
    };
    const messageId = str(entry.get("messageId"));
    if (messageId) result.messageId = messageId;
    const usage = record(entry.get("tokenUsage"));
    if (usage) result.tokenUsage = usage;
    return result;
}

function plan(value: unknown): PlanEntryView[] {
    return list(value, (part) => {
        const row = obj(part);
        const content = str(row?.content);
        if (!content) return null;
        const item: PlanEntryView = { content, status: str(row?.status) ?? "pending" };
        const activeForm = str(row?.activeForm);
        if (activeForm) item.activeForm = activeForm;
        return item;
    });
}

function taskView(value: unknown): TaskView | null {
    const task = map(value);
    const taskId = str(task?.get("taskId"));
    if (!task || !taskId) return null;
    const result: TaskView = {
        taskId, kind: str(task.get("kind")) ?? "background",
        title: str(task.get("title")) ?? "Task", status: str(task.get("status")) ?? "running",
    };
    for (const key of ["summary", "taskSubtype", "startedAt", "completedAt", "updatedAt"] as const) {
        const value = str(task.get(key));
        if (value) result[key] = value;
    }
    const durationMs = num(task.get("durationMs"));
    if (durationMs !== null) result.durationMs = durationMs;
    if (typeof task.get("isBackground") === "boolean") result.isBackground = task.get("isBackground") as boolean;
    return result;
}

function interactionView(value: unknown): InteractionView | null {
    const pending = obj(value);
    const id = num(pending?.id);
    const kind = str(pending?.kind);
    const sessionId = str(pending?.sessionId);
    if (id === null || (kind !== "permission" && kind !== "elicitation") || !sessionId) return null;
    const result: InteractionView = { id, kind, sessionId };
    for (const key of ["toolCallId", "title", "message"] as const) {
        const value = str(pending?.[key]);
        if (value) result[key] = value;
    }
    if (kind === "permission") result.options = list(pending?.options, (value) => {
        const row = obj(value); const optionId = str(row?.optionId);
        return optionId ? { optionId, name: str(row?.name) ?? optionId } : null;
    });
    if (kind === "elicitation") result.fields = list(pending?.fields, (value) => {
        const row = obj(value); const key = str(row?.key);
        return key ? { key, title: str(row?.title) ?? key, description: str(row?.description) ?? "" } : null;
    });
    return result;
}

function inputQueueView(value: unknown): InputQueueView | null {
    const queue = obj(value);
    const generation = str(queue?.generation);
    if (!generation) return null;
    const result: InputQueueView = {
        generation, revision: num(queue?.revision) ?? 0,
        items: list(queue?.items, (value) => {
            const item = obj(value); const inputId = str(item?.inputId); const state = str(item?.state);
            return inputId && state ? { inputId, state } : null;
        }),
    };
    const activeRequestId = str(queue?.activeRequestId);
    if (activeRequestId) result.activeRequestId = activeRequestId;
    return result;
}

function sessionInfo(value: Y.Map<unknown> | null): SessionInfoView {
    const result: SessionInfoView = {};
    for (const key of ["title", "model", "mode", "updatedAt", "activeRequestId", "activeTurnInteractionStatus", "compactionStatus", "suspendedTurnId"] as const) {
        const field = str(value?.get(key));
        if (field) result[key] = field;
    }
    const rewindRevision = num(value?.get("rewindRevision"));
    if (rewindRevision !== null) result.rewindRevision = rewindRevision;
    for (const key of ["modeState", "modelState", "goal", "stateSnapshotMeta", "compaction"] as const) {
        const field = record(value?.get(key));
        if (field) result[key] = field;
    }
    for (const key of ["configOptions", "availableCommands"] as const) {
        const field = value?.get(key);
        if (Array.isArray(field)) result[key] = field.map((item) => record(item)).filter((item): item is Record<string, unknown> => item !== null);
    }
    return result;
}

/** One read-only, transport-neutral view over an authorized pair of Y.Doc replicas. */
export function readSessionView(chat: Y.Doc, session: Y.Doc): SessionView {
    // A browser replica starts empty. Reading it must not create competing root keys.
    const chatRoot = root(chat);
    const sessionRoot = root(session);
    const entries = map(chatRoot?.get("entries"));
    const tools = map(chatRoot?.get("toolCalls"));
    const tasks = map(sessionRoot?.get("tasks"));
    const info = map(sessionRoot?.get("session"));
    const plans = map(sessionRoot?.get("plansByTurn"));
    const plansByTurn: Record<string, PlanEntryView[]> = Object.create(null);
    if (plans) for (const [turnId, value] of plans) plansByTurn[turnId] = plan(value);
    const pending = map(info?.get("pendingInteractions"));
    return {
        schemaVersion: num(chatRoot?.get("schemaVersion")) ?? 0,
        entries: list(chatRoot?.get("entryOrder"), (id) => entryView(entries?.get(String(id)), tools)),
        activeTurnId: str(info?.get("activeTurnId")),
        activeTurnStatus: str(info?.get("activeTurnStatus")),
        session: sessionInfo(info),
        tasks: list(sessionRoot?.get("taskOrder"), (id) => taskView(tasks?.get(String(id)))),
        plan: plan(info?.get("plan")),
        plansByTurn,
        pendingInteractions: pending ? Array.from(pending.values()).flatMap((item) => {
            const result = interactionView(item); return result ? [result] : [];
        }) : [],
        inputQueue: inputQueueView(info?.get("inputQueue")),
    };
}

/** A framework-neutral external store; one microtask coalesces updates to both documents. */
export class SessionViewStore {
    private listeners = new Set<() => void>();
    private snapshot: SessionView;
    private generation = 0;
    private pending = false;
    private closed = false;
    private readonly onUpdate = () => this.schedule();

    constructor(private chat: Y.Doc, private session: Y.Doc) {
        this.snapshot = readSessionView(chat, session);
        this.attach();
    }

    getSnapshot(): SessionView { return this.snapshot; }
    subscribe(listener: () => void): () => void {
        if (this.closed) return () => {};
        this.listeners.add(listener);
        return () => this.listeners.delete(listener);
    }
    replaceDocs(chat: Y.Doc, session: Y.Doc): void {
        if (this.closed) return;
        this.detach();
        this.generation++;
        this.pending = false;
        this.chat = chat;
        this.session = session;
        this.snapshot = readSessionView(chat, session);
        this.attach();
        this.emit();
    }
    destroy(): void {
        if (this.closed) return;
        this.closed = true;
        this.generation++;
        this.detach();
        this.listeners.clear();
    }
    private attach(): void { this.chat.on("update", this.onUpdate); this.session.on("update", this.onUpdate); }
    private detach(): void { this.chat.off("update", this.onUpdate); this.session.off("update", this.onUpdate); }
    private emit(): void { for (const listener of this.listeners) listener(); }
    private schedule(): void {
        if (this.pending || this.closed) return;
        this.pending = true;
        const generation = this.generation;
        queueMicrotask(() => {
            if (this.closed || generation !== this.generation) return;
            this.pending = false;
            this.snapshot = readSessionView(this.chat, this.session);
            this.emit();
        });
    }
}
