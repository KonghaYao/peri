import * as Y from "yjs";
import { CHAT_SCHEMA_VERSION, SESSION_SCHEMA_VERSION, type PublicError } from "../state/doc-model";
import { ReadCache } from "./read-cache";
import type { ToolPayloadRef } from "../state/tool-payloads";

export type ToolView = {
    toolCallId: string;
    turnId: string | null;
    entryId?: string;
    name: string;
    status: string;
    kind?: string;
    permissionId?: string;
    createdAt: string | null;
    completedAt: string | null;
    error: PublicError | null;
    arguments?: unknown;
    result?: unknown;
    argumentsRef?: ToolPayloadRef;
    resultRef?: ToolPayloadRef;
};
export type EntryBlockView =
    | { blockId: string; type: "text" | "reasoning"; text: string; visibility?: string }
    | { blockId: string; type: "tool_call"; toolCallId: string; tool: ToolView | null; unavailable?: string }
    | { blockId: string; type: "unknown"; reason: string };
export type EntryView = {
    entryId: string;
    id: string;
    kind: "message" | "tool" | "system";
    turnId: string;
    role: "user" | "assistant";
    status: string;
    messageId?: string;
    createdAt: string | null;
    createdAtSource: string;
    completedAt: string | null;
    error: PublicError | null;
    tokenUsage?: Record<string, unknown>;
    blocks: EntryBlockView[];
};
export type TurnView = {
    turnId: string;
    status: string;
    requestId?: string;
    startedAt: string | null;
    completedAt: string | null;
    error: PublicError | null;
    stopReason?: string;
    interactionStatus: string;
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
export type InteractionFieldView = {
    key: string;
    title: string;
    description: string;
    type: "string" | "number" | "integer" | "boolean";
    required: boolean;
    enum?: Array<string | number | boolean>;
};
export type InteractionView = {
    id: string;
    kind: "permission" | "elicitation";
    sessionId: string;
    turnId?: string;
    toolCallId?: string;
    status: "pending" | "resolved" | "expired";
    createdAt: string | null;
    resolvedAt: string | null;
    outcome?: string;
    title?: string;
    message?: string;
    formUnsupported?: string;
    options?: Array<{ optionId: string; name: string }>;
    fields?: InteractionFieldView[];
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
    legacyIncomplete?: boolean;
    rewindRevision?: number;
    modeState?: Record<string, unknown>;
    modelState?: Record<string, unknown>;
    goal?: Record<string, unknown>;
    stateSnapshotMeta?: Record<string, unknown>;
    compaction?: Record<string, unknown>;
    configOptions?: Array<Record<string, unknown>>;
    availableCommands?: Array<Record<string, unknown>>;
};
/** not-ready：副本尚无完整文档对；unsupported：文档对完整但版本不受支持，须升级而非当作空历史。 */
export type SessionViewReadiness = "not-ready" | "ready" | "unsupported";
export type SessionView = {
    readiness: SessionViewReadiness;
    schemaVersions: { chat: number | null; session: number | null };
    entries: EntryView[];
    toolCalls: ToolView[];
    turns: TurnView[];
    activeTurn: { turnId: string | null; status: string | null };
    session: SessionInfoView;
    tasks: TaskView[];
    plan: PlanEntryView[];
    plansByTurn: Record<string, PlanEntryView[]>;
    interactions: InteractionView[];
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
function timeOrNull(value: unknown): string | null { return typeof value === "string" && value.length > 0 ? value : null; }
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

function publicErrorView(value: unknown): PublicError | null {
    const parsed = obj(value);
    const code = str(parsed?.code);
    const message = str(parsed?.message);
    return code && message ? { code: code as PublicError["code"], message } : null;
}

function toolView(value: unknown, cache: ReadCache): ToolView | null {
    const tool = map(value);
    const toolCallId = str(tool?.get("toolCallId"));
    if (!tool || !toolCallId) return null;
    return cache.read(tool, () => {
        const result: ToolView = {
            toolCallId, turnId: str(tool.get("turnId")), name: str(tool.get("name")) ?? "Tool",
            status: str(tool.get("status")) ?? "pending",
            createdAt: timeOrNull(tool.get("createdAt")), completedAt: timeOrNull(tool.get("completedAt")),
            error: publicErrorView(tool.get("error")),
        };
        const entryId = str(tool.get("entryId"));
        if (entryId) result.entryId = entryId;
        const kind = str(tool.get("kind"));
        if (kind) result.kind = kind;
        const permissionId = str(tool.get("permissionId"));
        if (permissionId) result.permissionId = permissionId;
        if (tool.has("arguments")) result.arguments = copy(tool.get("arguments"));
        if (tool.has("result")) result.result = copy(tool.get("result"));
        for (const key of ["argumentsRef", "resultRef"] as const) {
            const ref = obj(tool.get(key));
            if (ref && typeof ref.id === "string" && typeof ref.bytes === "number" && typeof ref.preview === "string") {
                result[key] = { id: ref.id, bytes: ref.bytes, preview: ref.preview };
            }
        }
        return result;
    });
}

function blockView(value: unknown, tools: Y.Map<unknown> | null, cache: ReadCache): EntryBlockView | null {
    const block = map(value);
    const blockId = str(block?.get("blockId"));
    const type = str(block?.get("type"));
    if (!block || !blockId) return null;
    return cache.read(block, () => {
        if (type === "text" || type === "reasoning") {
            const text = block.get("text");
            const result: EntryBlockView = { blockId, type, text: text instanceof Y.Text ? text.toString() : str(text) ?? "" };
            const visibility = str(block.get("visibility"));
            if (visibility && type === "reasoning") result.visibility = visibility;
            return result;
        }
        if (type === "tool_call") {
            const toolCallId = str(block.get("toolCallId"));
            if (toolCallId) {
                const tool = map(tools?.get(toolCallId));
                cache.depend(tool ?? tools, block);
                // 孤立引用不静默隐藏：块保留并带明确原因。
                return { blockId, type: "tool_call", toolCallId, tool: toolView(tool, cache),
                    ...(tool ? {} : { unavailable: "missing_tool" }) };
            }
        }
        // 未知或不支持的块（含未核实的 resource）保留占位与原因，不按空内容吞掉。
        return { blockId, type: "unknown", reason: type ? `unsupported_block:${type}` : "unknown_block" };
    });
}

function entryView(value: unknown, tools: Y.Map<unknown> | null, cache: ReadCache): EntryView | null {
    const entry = map(value);
    const entryId = str(entry?.get("entryId"));
    const turnId = str(entry?.get("turnId"));
    const role = str(entry?.get("role"));
    if (!entry || !entryId || !turnId || (role !== "user" && role !== "assistant")) return null;
    return cache.read(entry, () => {
        const blocks = map(entry.get("blocks"));
        const kind = str(entry.get("kind"));
        const result: EntryView = {
            entryId, id: str(entry.get("id")) ?? entryId,
            kind: kind === "tool" || kind === "system" ? kind : "message",
            turnId, role, status: str(entry.get("status")) ?? "pending",
            createdAt: timeOrNull(entry.get("createdAt")),
            createdAtSource: str(entry.get("createdAtSource")) ?? "unknown",
            completedAt: timeOrNull(entry.get("completedAt")),
            error: publicErrorView(entry.get("error")),
            blocks: list(entry.get("blockOrder"), (id) => blockView(blocks?.get(String(id)), tools, cache)),
        };
        const messageId = str(entry.get("messageId"));
        if (messageId) result.messageId = messageId;
        const usage = record(entry.get("tokenUsage"));
        if (usage) result.tokenUsage = usage;
        return result;
    });
}

function turnView(value: unknown): TurnView | null {
    const turn = map(value);
    const turnId = str(turn?.get("turnId"));
    if (!turn || !turnId) return null;
    const result: TurnView = {
        turnId, status: str(turn.get("status")) ?? "completed",
        startedAt: timeOrNull(turn.get("startedAt")), completedAt: timeOrNull(turn.get("completedAt")),
        error: publicErrorView(turn.get("error")),
        interactionStatus: str(turn.get("interactionStatus")) ?? "none",
    };
    const requestId = str(turn.get("requestId"));
    if (requestId) result.requestId = requestId;
    const stopReason = str(turn.get("stopReason"));
    if (stopReason) result.stopReason = stopReason;
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
    const record = map(value);
    const id = str(record?.get("id"));
    const kind = str(record?.get("kind"));
    const sessionId = str(record?.get("sessionId"));
    if (!record || !id || (kind !== "permission" && kind !== "elicitation") || !sessionId) return null;
    const status = str(record.get("status"));
    const result: InteractionView = {
        id, kind, sessionId,
        status: status === "resolved" || status === "expired" ? status : "pending",
        createdAt: timeOrNull(record.get("createdAt")),
        resolvedAt: timeOrNull(record.get("resolvedAt")),
    };
    for (const key of ["turnId", "toolCallId", "title", "message", "outcome"] as const) {
        const field = str(record.get(key));
        if (field) result[key] = field;
    }
    if (record.get("formSupport") === "unsupported") result.formUnsupported = str(record.get("formReason")) ?? "unsupported_form";
    if (kind === "permission") result.options = list(record.get("options"), (value) => {
        const row = obj(value); const optionId = str(row?.optionId);
        return optionId ? { optionId, name: str(row?.name) ?? optionId } : null;
    });
    if (kind === "elicitation") result.fields = list(record.get("fields"), (value) => {
        const row = obj(value); const key = str(row?.key);
        const type = str(row?.type);
        if (!key || (type !== "string" && type !== "number" && type !== "integer" && type !== "boolean")) return null;
        const options = Array.isArray(row?.enum)
            ? row!.enum.filter((option): option is string | number | boolean =>
                typeof option === "string" || typeof option === "number" || typeof option === "boolean") : undefined;
        return {
            key, title: str(row?.title) ?? key, description: str(row?.description) ?? "", type,
            required: row?.required === true, ...(options?.length ? { enum: options } : {}),
        };
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
    for (const key of ["title", "model", "mode", "updatedAt", "activeRequestId", "compactionStatus", "suspendedTurnId"] as const) {
        const field = str(value?.get(key));
        if (field) result[key] = field;
    }
    if (value?.get("legacyIncomplete") === true) result.legacyIncomplete = true;
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
    return readView(chat, session, new ReadCache());
}

function readView(chat: Y.Doc, session: Y.Doc, cache: ReadCache): SessionView {
    // A browser replica starts empty. Reading it must not create competing root keys.
    const chatRoot = root(chat);
    const sessionRoot = root(session);
    const schemaVersions = {
        chat: num(chatRoot?.get("schemaVersion")),
        session: num(sessionRoot?.get("schemaVersion")),
    };
    const readiness: SessionViewReadiness = !chatRoot || !sessionRoot ? "not-ready"
        : schemaVersions.chat === CHAT_SCHEMA_VERSION && schemaVersions.session === SESSION_SCHEMA_VERSION ? "ready"
        : "unsupported";
    const empty: SessionView = {
        readiness, schemaVersions, entries: [], toolCalls: [], turns: [],
        activeTurn: { turnId: null, status: null }, session: {}, tasks: [], plan: [], plansByTurn: {},
        interactions: [], pendingInteractions: [], inputQueue: null,
    };
    // 不支持或未完成的文档对不得伪装成空历史：调用方必须按 readiness 分支。
    if (readiness !== "ready") return empty;

    const entries = map(chatRoot?.get("entries"));
    const tools = map(chatRoot?.get("toolCalls"));
    const turns = map(sessionRoot?.get("turns"));
    const interactions = map(sessionRoot?.get("interactions"));
    const tasks = map(sessionRoot?.get("tasks"));
    const info = map(sessionRoot?.get("session"));
    const plans = map(sessionRoot?.get("plansByTurn"));
    const plansByTurn = cache.read(plans, () => {
        const result: Record<string, PlanEntryView[]> = Object.create(null);
        if (plans) for (const [turnId, value] of plans) result[turnId] = plan(value);
        return result;
    });
    if (tasks) cache.depend(array(sessionRoot?.get("taskOrder")), tasks);
    const interactionViews = cache.read(interactions, () => interactions
        ? Array.from(interactions.values()).flatMap((item) => { const parsed = interactionView(item); return parsed ? [parsed] : []; })
        : []);
    const pendingInteractions = interactionViews.filter((item) => item.status === "pending");
    const activeTurnId = str(info?.get("activeTurnId"));
    const derivedInteractionStatus = (() => {
        if (!activeTurnId) return undefined;
        let status: string | undefined;
        for (const item of pendingInteractions) {
            if (item.turnId !== activeTurnId) continue;
            if (item.kind === "permission") return "awaiting_permission";
            status = "awaiting_input";
        }
        return status;
    })();
    const sessionInfoView = { ...cache.read(info, () => sessionInfo(info)) };
    if (derivedInteractionStatus) sessionInfoView.activeTurnInteractionStatus = derivedInteractionStatus;
    return {
        readiness,
        schemaVersions,
        entries: cache.read(chatRoot, () => list(chatRoot?.get("entryOrder"), (id) => entryView(entries?.get(String(id)), tools, cache))),
        toolCalls: cache.read(tools, () => tools
            ? Array.from(tools.values()).flatMap((item) => { const parsed = toolView(item, cache); return parsed ? [parsed] : []; })
            : []),
        turns: cache.read(turns, () => turns
            ? Array.from(turns.values()).flatMap((item) => { const parsed = turnView(item); return parsed ? [parsed] : []; })
            : []),
        activeTurn: { turnId: activeTurnId, status: str(info?.get("activeTurnStatus")) },
        session: sessionInfoView,
        tasks: cache.read(tasks, () => list(sessionRoot?.get("taskOrder"), (id) => taskView(tasks?.get(String(id))))),
        plan: plan(info?.get("plan")),
        plansByTurn,
        interactions: interactionViews,
        pendingInteractions,
        inputQueue: inputQueueView(info?.get("inputQueue")),
    };
}

/** 纯查询：按身份取 Entry；未找到返回 null。 */
export function selectEntry(view: SessionView, entryId: string): EntryView | null {
    return view.entries.find((entry) => entry.entryId === entryId || entry.id === entryId) ?? null;
}
/** 纯查询：按身份取独立 ToolCall；未找到返回 null。 */
export function selectToolCall(view: SessionView, toolCallId: string): ToolView | null {
    return view.toolCalls.find((tool) => tool.toolCallId === toolCallId) ?? null;
}
/** 纯查询：按身份取 Turn；未找到返回 null。 */
export function selectTurn(view: SessionView, turnId: string): TurnView | null {
    return view.turns.find((turn) => turn.turnId === turnId) ?? null;
}
/** 纯查询：按 turn 取计划；该 turn 无计划记录返回 null，区别于空计划。 */
export function selectPlanForTurn(view: SessionView, turnId: string): PlanEntryView[] | null {
    return view.plansByTurn[turnId] ?? null;
}
/** 纯查询：未决交互（派生视图，不新增缓存）。 */
export function selectPendingInteractions(view: SessionView): InteractionView[] {
    return view.pendingInteractions;
}

/** A framework-neutral external store; one microtask coalesces updates to both documents. */
export class SessionViewStore {
    private listeners = new Set<() => void>();
    private snapshot: SessionView;
    private generation = 0;
    private pending = false;
    private closed = false;
    private cache = new ReadCache();
    private readonly onTransaction = (transaction: Y.Transaction) => {
        if (!transaction.changed.size) return;
        // Replacing a root collection also replaces reference targets beneath that collection.
        if ([...transaction.changed.keys()].some((type) => type.parent === null)) this.cache = new ReadCache();
        else this.cache.invalidate(transaction);
        this.schedule();
    };

    constructor(private chat: Y.Doc, private session: Y.Doc) {
        this.snapshot = readView(chat, session, this.cache);
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
        this.cache = new ReadCache();
        this.snapshot = readView(chat, session, this.cache);
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
    private attach(): void { this.chat.on("afterTransaction", this.onTransaction); this.session.on("afterTransaction", this.onTransaction); }
    private detach(): void { this.chat.off("afterTransaction", this.onTransaction); this.session.off("afterTransaction", this.onTransaction); }
    private emit(): void { for (const listener of this.listeners) listener(); }
    private schedule(): void {
        if (this.pending || this.closed) return;
        this.pending = true;
        const generation = this.generation;
        queueMicrotask(() => {
            if (this.closed || generation !== this.generation) return;
            this.pending = false;
            this.snapshot = readView(this.chat, this.session, this.cache);
            this.emit();
        });
    }
}
