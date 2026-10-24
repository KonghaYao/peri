import * as Y from "yjs";
import {
    CHAT_SCHEMA_VERSION, DocModel, SESSION_SCHEMA_VERSION, isPublicErrorCode, isTerminal, publicError,
    type PublicError,
} from "./doc-model";
import { object, string } from "./protocol";
import type { ToolPayloadRef } from "./tool-payloads";
import { turnInteractionStatusFor } from "./interaction-projection";

/** checkpoint 自身形状的版本；与 chat/session 文档 schema 分开管理。v2 起只承载展示材料，不含载荷全文。 */
export const CHECKPOINT_VERSION = 2;
export const SUPPORTED_SCHEMA_VERSIONS = { chat: CHAT_SCHEMA_VERSION, session: SESSION_SCHEMA_VERSION } as const;

export type CheckpointBlock =
    | { blockId: string; type: "text"; text: string }
    | { blockId: string; type: "reasoning"; text: string; visibility: "summary" | "hidden" }
    | { blockId: string; type: "tool_call"; toolCallId: string };

export type CheckpointEntry = {
    entryId: string;
    id: string;
    kind: "message" | "tool" | "system";
    turnId: string | null;
    role: "user" | "assistant";
    status: string;
    messageId?: string;
    tokenUsage?: Record<string, unknown>;
    createdAt: string | null;
    createdAtSource: "source" | "observed" | "unknown";
    completedAt: string | null;
    error: PublicError | null;
    blocks: CheckpointBlock[];
};

export type CheckpointToolCall = {
    toolCallId: string;
    entryId?: string;
    turnId: string;
    name: string;
    kind?: string;
    status: string;
    createdAt: string | null;
    completedAt: string | null;
    error: PublicError | null;
    permissionId?: string;
    arguments?: unknown;
    argumentsRef?: ToolPayloadRef;
    result?: unknown;
    resultRef?: ToolPayloadRef;
};

export type CheckpointTurn = {
    turnId: string;
    requestId?: string;
    status: string;
    startedAt: string | null;
    completedAt: string | null;
    error: PublicError | null;
    stopReason?: string;
    interactionStatus: "none" | "awaiting_permission" | "awaiting_input";
};

export type CheckpointInteraction = {
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
    options?: Array<{ optionId: string; name: string }>;
    fields?: Array<{ key: string; title: string; description: string }>;
};

/** 纯展示材料：无 resolver、无事件去重集合、无执行恢复状态。 */
export type DisplayCheckpoint = {
    checkpointVersion: number;
    schemaVersions: { chat: number; session: number };
    legacyIncomplete?: true;
    session: Record<string, unknown>;
    entries: CheckpointEntry[];
    toolCalls: CheckpointToolCall[];
    turns: CheckpointTurn[];
    tasks: Array<Record<string, unknown>>;
    plansByTurn: Record<string, unknown>;
    interactions: CheckpointInteraction[];
    inputQueue?: unknown;
};

export type CheckpointImportErrorCode =
    | "unsupported_version" | "unsupported_schema" | "invalid_shape" | "broken_reference";

export class CheckpointImportError extends Error {
    constructor(readonly code: CheckpointImportErrorCode, message: string) {
        super(message);
        this.name = "CheckpointImportError";
    }
}

const SESSION_INFO_STRINGS = ["title", "model", "mode", "updatedAt", "compactionStatus", "suspendedTurnId"] as const;
const SESSION_INFO_JSON = ["modeState", "modelState", "goal", "stateSnapshotMeta", "compaction", "configOptions", "availableCommands", "plan"] as const;
const TASK_TERMINAL = new Set(["completed", "failed", "cancelled"]);

function plain(value: unknown): unknown {
    if (value instanceof Y.AbstractType) return plain(value.toJSON());
    if (Array.isArray(value)) return value.map(plain);
    const record = object(value);
    if (record) return Object.fromEntries(Object.entries(record).map(([key, part]) => [key, plain(part)]));
    return value;
}
function rawText(value: unknown): string {
    return value instanceof Y.Text ? value.toString() : typeof value === "string" ? value : "";
}
function timeOf(value: unknown): string | null {
    return typeof value === "string" && value.length > 0 ? value : null;
}
function sourceOf(value: unknown): CheckpointEntry["createdAtSource"] {
    return value === "source" || value === "observed" ? value : "unknown";
}
function publicErrorOf(value: unknown): PublicError | null {
    if (value === null || value === undefined) return null;
    const record = object(value);
    if (!record || !isPublicErrorCode(record.code) || typeof record.message !== "string") {
        throw new CheckpointImportError("invalid_shape", "error 必须是白名单 PublicError");
    }
    return { code: record.code, message: record.message };
}
function refOf(value: unknown): ToolPayloadRef | null {
    const ref = object(value);
    const id = string(ref?.id);
    if (!ref || !id || typeof ref.bytes !== "number" || !Number.isFinite(ref.bytes) || typeof ref.preview !== "string") return null;
    return { id, bytes: ref.bytes, preview: ref.preview };
}

/** 导出一个文档对的公开展示材料；payload 全文随索引一起导出，不丢弃可解析性。 */
export function exportCheckpoint(model: DocModel): DisplayCheckpoint {
    const info = model.info();
    const session: Record<string, unknown> = {};
    for (const key of SESSION_INFO_STRINGS) {
        const value = string(info.get(key));
        if (value) session[key] = value;
    }
    for (const key of SESSION_INFO_JSON) {
        const value = info.get(key);
        if (value !== undefined) session[key] = plain(value);
    }
    const rewind = info.get("rewindRevision");
    if (typeof rewind === "number" && Number.isFinite(rewind)) session.rewindRevision = rewind;
    const sessionId = string(info.get("sessionId"));
    if (sessionId) session.sessionId = sessionId;
    const bindingRevision = info.get("bindingRevision");
    if (typeof bindingRevision === "number" && Number.isFinite(bindingRevision)) session.bindingRevision = bindingRevision;
    for (const key of ["running", "executionBlocked"] as const) {
        if (typeof info.get(key) === "boolean") session[key] = info.get(key);
    }
    if (info.get("closed") === true) session.closed = true;
    const closedReason = publicErrorOf(info.get("closedReason"));
    if (closedReason) session.closedReason = closedReason;

    const entries: CheckpointEntry[] = [];
    for (const id of (model.chatRoot().get("entryOrder") as Y.Array<string>).toArray()) {
        const entry = model.entries().get(id);
        if (!entry) continue;
        const kind = entry.get("kind");
        const item: CheckpointEntry = {
            entryId: string(entry.get("entryId")) ?? id,
            id: string(entry.get("id")) ?? string(entry.get("entryId")) ?? id,
            kind: kind === "tool" || kind === "system" ? kind : "message",
            turnId: timeOf(entry.get("turnId")),
            role: entry.get("role") === "user" ? "user" : "assistant",
            status: string(entry.get("status")) ?? "pending",
            createdAt: timeOf(entry.get("createdAt")),
            createdAtSource: sourceOf(entry.get("createdAtSource")),
            completedAt: timeOf(entry.get("completedAt")),
            error: publicErrorOf(entry.get("error")),
            blocks: exportBlocks(entry),
        };
        const messageId = string(entry.get("messageId"));
        if (messageId) item.messageId = messageId;
        const usage = object(entry.get("tokenUsage"));
        if (usage) item.tokenUsage = plain(usage) as Record<string, unknown>;
        entries.push(item);
    }

    const toolCalls: CheckpointToolCall[] = [];
    for (const [id, tool] of model.tools()) {
        const item: CheckpointToolCall = {
            toolCallId: string(tool.get("toolCallId")) ?? id,
            turnId: string(tool.get("turnId")) ?? "",
            name: string(tool.get("name")) ?? "Tool",
            status: string(tool.get("status")) ?? "pending",
            createdAt: timeOf(tool.get("createdAt")),
            completedAt: timeOf(tool.get("completedAt")),
            error: publicErrorOf(tool.get("error")),
        };
        const entryId = string(tool.get("entryId"));
        if (entryId) item.entryId = entryId;
        const kind = string(tool.get("kind"));
        if (kind) item.kind = kind;
        const permissionId = string(tool.get("permissionId"));
        if (permissionId) item.permissionId = permissionId;
        if (tool.has("arguments")) item.arguments = plain(tool.get("arguments"));
        if (tool.has("result")) item.result = plain(tool.get("result"));
        const argumentsRef = refOf(tool.get("argumentsRef"));
        if (argumentsRef) item.argumentsRef = argumentsRef;
        const resultRef = refOf(tool.get("resultRef"));
        if (resultRef) item.resultRef = resultRef;
        toolCalls.push(item);
    }

    const turns: CheckpointTurn[] = [];
    for (const [, turn] of model.turns()) {
        const item: CheckpointTurn = {
            turnId: string(turn.get("turnId")) ?? "",
            status: string(turn.get("status")) ?? "completed",
            startedAt: timeOf(turn.get("startedAt")),
            completedAt: timeOf(turn.get("completedAt")),
            error: publicErrorOf(turn.get("error")),
            interactionStatus: turn.get("interactionStatus") === "awaiting_permission" ? "awaiting_permission"
                : turn.get("interactionStatus") === "awaiting_input" ? "awaiting_input" : "none",
        };
        const requestId = string(turn.get("requestId"));
        if (requestId) item.requestId = requestId;
        const stopReason = string(turn.get("stopReason"));
        if (stopReason) item.stopReason = stopReason;
        turns.push(item);
    }

    const interactions: CheckpointInteraction[] = [];
    for (const [, record] of model.interactions()) {
        const kind = record.get("kind") === "elicitation" ? "elicitation" : "permission";
        const item: CheckpointInteraction = {
            id: string(record.get("id")) ?? "",
            kind,
            sessionId: string(record.get("sessionId")) ?? "",
            status: record.get("status") === "pending" ? "pending" : record.get("status") === "resolved" ? "resolved" : "expired",
            createdAt: timeOf(record.get("createdAt")),
            resolvedAt: timeOf(record.get("resolvedAt")),
        };
        for (const key of ["turnId", "toolCallId", "title", "message", "outcome"] as const) {
            const value = string(record.get(key));
            if (value) item[key] = value;
        }
        const options = record.get("options");
        if (Array.isArray(options)) item.options = options.flatMap((raw) => {
            const option = object(raw);
            const optionId = string(option?.optionId);
            return optionId ? [{ optionId, name: string(option?.name) ?? optionId }] : [];
        });
        const fields = record.get("fields");
        if (Array.isArray(fields)) item.fields = fields.flatMap((raw) => {
            const field = object(raw);
            const key = string(field?.key);
            return key ? [{ key, title: string(field?.title) ?? key, description: string(field?.description) ?? "" }] : [];
        });
        interactions.push(item);
    }

    const tasks: Array<Record<string, unknown>> = [];
    for (const id of (model.sessionRoot().get("taskOrder") as Y.Array<string>).toArray()) {
        const task = model.tasks().get(id);
        const record = task ? object(plain(task.toJSON())) : null;
        if (record) tasks.push(record);
    }

    const plansByTurn: Record<string, unknown> = {};
    const plans = model.sessionRoot().get("plansByTurn") as Y.Map<unknown> | undefined;
    if (plans) for (const [turnId, value] of plans) plansByTurn[turnId] = plain(value);

    const checkpoint: DisplayCheckpoint = {
        checkpointVersion: CHECKPOINT_VERSION,
        schemaVersions: {
            chat: typeof model.chatRoot().get("schemaVersion") === "number" ? model.chatRoot().get("schemaVersion") as number : 0,
            session: typeof model.sessionRoot().get("schemaVersion") === "number" ? model.sessionRoot().get("schemaVersion") as number : 0,
        },
        session,
        entries,
        toolCalls,
        turns,
        tasks,
        plansByTurn,
        interactions,
    };
    const queue = info.get("inputQueue");
    if (queue !== undefined) checkpoint.inputQueue = plain(queue);
    if (info.get("legacyIncomplete") === true) checkpoint.legacyIncomplete = true;
    return checkpoint;
}

function exportBlocks(entry: Y.Map<unknown>): CheckpointBlock[] {
    const order = entry.get("blockOrder");
    const blocks = entry.get("blocks");
    if (!(order instanceof Y.Array) || !(blocks instanceof Y.Map)) return [];
    const result: CheckpointBlock[] = [];
    for (const blockId of order.toArray()) {
        const block = (blocks as Y.Map<Y.Map<unknown>>).get(String(blockId));
        if (!block) continue;
        const type = block.get("type");
        if (type === "text" || type === "reasoning") {
            const text = rawText(block.get("text"));
            result.push(type === "reasoning"
                ? { blockId: String(blockId), type, text, visibility: block.get("visibility") === "hidden" ? "hidden" : "summary" }
                : { blockId: String(blockId), type, text });
        } else if (type === "tool_call") {
            const toolCallId = string(block.get("toolCallId"));
            if (toolCallId) result.push({ blockId: String(blockId), type: "tool_call", toolCallId });
        }
    }
    return result;
}

/**
 * 把 checkpoint 应用到全新文档：重建索引、清除 live 身份，活动 turn/interactions 收敛为中断/失效。
 * 只能作用于未承载内容的 DocModel；不做 Yjs update 重放。
 */
export function applyCheckpoint(raw: unknown, model: DocModel): void {
    const fail = (code: CheckpointImportErrorCode, message: string): never => {
        throw new CheckpointImportError(code, message);
    };
    const listOf = (value: unknown, label: string): unknown[] => {
        if (!Array.isArray(value)) fail("invalid_shape", `${label} 必须是数组`);
        return value as unknown[];
    };

    const checkpoint = object(raw) ?? fail("invalid_shape", "checkpoint 必须是 JSON 对象");
    if (checkpoint.checkpointVersion !== CHECKPOINT_VERSION) {
        fail("unsupported_version", `不支持的 checkpointVersion: ${String(checkpoint.checkpointVersion)}`);
    }
    const versions = object(checkpoint.schemaVersions);
    if (versions?.chat !== SUPPORTED_SCHEMA_VERSIONS.chat || versions?.session !== SUPPORTED_SCHEMA_VERSIONS.session) {
        fail("unsupported_schema", `不支持的文档 schema pair: ${JSON.stringify(versions ?? null)}`);
    }
    const entriesRaw = listOf(checkpoint.entries, "entries");
    const toolCallsRaw = listOf(checkpoint.toolCalls, "toolCalls");
    const turnsRaw = listOf(checkpoint.turns, "turns");
    const tasksRaw = listOf(checkpoint.tasks, "tasks");
    const interactionsRaw = listOf(checkpoint.interactions, "interactions");

    // 1. 展示 checkpoint 只恢复引用形状；载荷全文由运行侧 PayloadStore 持有，不进入副本。
    const resolveRef = (rawRef: unknown, label: string): ToolPayloadRef =>
        refOf(rawRef) ?? fail("broken_reference", `${label} 的载荷引用形状非法`);
    const inlineJson = (value: unknown, label: string): unknown => {
        const json = JSON.stringify(value);
        if (json === undefined) fail("invalid_shape", `${label} 不是合法 JSON`);
        return JSON.parse(json as string);
    };

    // 2. 工具先于 Entry：tool_call 块要能引用已存在的工具身份。
    model.chat.transact(() => {
        for (const item of toolCallsRaw) {
            const rawTool = object(item) ?? fail("invalid_shape", "toolCalls 条目必须是对象");
            const toolCallId = string(rawTool.toolCallId) ?? fail("invalid_shape", "toolCalls 缺少 toolCallId");
            const turnId = string(rawTool.turnId) ?? fail("invalid_shape", `工具 ${toolCallId} 缺少 turnId`);
            if (model.tools().has(toolCallId)) fail("invalid_shape", `toolCallId ${toolCallId} 重复`);
            const label = `工具 ${toolCallId}`;
            const tool = model.ensureTool(toolCallId, turnId, string(rawTool.name) ?? "Tool", true);
            tool.set("status", string(rawTool.status) ?? "pending");
            tool.set("createdAt", timeOf(rawTool.createdAt));
            tool.set("completedAt", timeOf(rawTool.completedAt));
            tool.set("error", publicErrorOf(rawTool.error));
            const kind = string(rawTool.kind);
            if (kind) tool.set("kind", kind);
            const entryId = string(rawTool.entryId);
            if (entryId) tool.set("entryId", entryId);
            const permissionId = string(rawTool.permissionId);
            if (permissionId) tool.set("permissionId", permissionId);
            if (rawTool.arguments !== undefined) tool.set("arguments", inlineJson(rawTool.arguments, `${label} arguments`));
            else if (rawTool.argumentsRef !== undefined) tool.set("argumentsRef", resolveRef(rawTool.argumentsRef, `${label} arguments`));
            if (rawTool.result !== undefined) tool.set("result", inlineJson(rawTool.result, `${label} result`));
            else if (rawTool.resultRef !== undefined) tool.set("resultRef", resolveRef(rawTool.resultRef, `${label} result`));
        }

        // 3. Entry 与有序块；turnId 缺失的历史条目使用占位键，view 不因缺 turn 键而隐藏。
        for (const item of entriesRaw) {
            const rawEntry = object(item) ?? fail("invalid_shape", "entries 条目必须是对象");
            const entryId = string(rawEntry.entryId) ?? fail("invalid_shape", "entries 缺少 entryId");
            const role = rawEntry.role === "user" || rawEntry.role === "assistant" ? rawEntry.role
                : fail("invalid_shape", `Entry ${entryId} 的 role 非法`);
            if (model.entries().has(entryId)) fail("invalid_shape", `entryId ${entryId} 重复`);
            const turnId = string(rawEntry.turnId) ?? `checkpoint:${entryId}`;
            const entry = model.ensureEntry(entryId, turnId, role, true);
            entry.set("id", string(rawEntry.id) ?? entryId);
            const kind = rawEntry.kind;
            entry.set("kind", kind === "tool" || kind === "system" ? kind : "message");
            entry.set("status", string(rawEntry.status) ?? (role === "user" ? "completed" : "pending"));
            entry.set("createdAt", timeOf(rawEntry.createdAt));
            entry.set("createdAtSource", sourceOf(rawEntry.createdAtSource));
            entry.set("completedAt", timeOf(rawEntry.completedAt));
            entry.set("error", publicErrorOf(rawEntry.error));
            const messageId = string(rawEntry.messageId);
            if (messageId) entry.set("messageId", messageId);
            const usage = object(rawEntry.tokenUsage);
            if (usage) entry.set("tokenUsage", plain(usage));
            for (const rawBlock of Array.isArray(rawEntry.blocks) ? rawEntry.blocks : []) {
                const block = object(rawBlock) ?? fail("invalid_shape", `Entry ${entryId} 的块必须是对象`);
                const blockId = string(block.blockId) ?? fail("invalid_shape", `Entry ${entryId} 的块缺少 blockId`);
                const type = string(block.type);
                if (type === "text" || type === "reasoning") {
                    if (typeof block.text !== "string") fail("invalid_shape", `Entry ${entryId} 的 ${type} 块缺少文本`);
                    model.appendText(entry, type, block.text as string);
                    if (type === "reasoning" && block.visibility === "hidden") {
                        const order = entry.get("blockOrder") as Y.Array<string>;
                        const blocks = entry.get("blocks") as Y.Map<Y.Map<unknown>>;
                        blocks.get(order.get(order.length - 1))?.set("visibility", "hidden");
                    }
                } else if (type === "tool_call") {
                    const toolCallId = string(block.toolCallId) ?? fail("invalid_shape", `Entry ${entryId} 的 tool_call 块缺少 toolCallId`);
                    if (!model.tools().has(toolCallId)) fail("broken_reference", `Entry ${entryId} 引用未知工具 ${toolCallId}`);
                    model.addToolBlock(entry, toolCallId);
                } else {
                    fail("invalid_shape", `Entry ${entryId} 的块类型 ${String(block.type)} 不受支持 (${blockId})`);
                }
            }
        }
    });

    // 4. session 公开材料；活动索引与请求身份不恢复。
    const sessionInfo = object(checkpoint.session) ?? {};
    model.session.transact(() => {
        const info = model.info();
        for (const key of SESSION_INFO_STRINGS) {
            const value = string(sessionInfo[key]);
            if (value) info.set(key, value);
        }
        for (const key of SESSION_INFO_JSON) {
            const value = sessionInfo[key];
            if (value !== undefined) info.set(key, plain(value));
        }
        const rewind = sessionInfo.rewindRevision;
        if (typeof rewind === "number" && Number.isFinite(rewind)) info.set("rewindRevision", rewind);
        const sessionId = string(sessionInfo.sessionId);
        if (sessionId) info.set("sessionId", sessionId);
        const bindingRevision = sessionInfo.bindingRevision;
        if (typeof bindingRevision === "number" && Number.isFinite(bindingRevision)) info.set("bindingRevision", bindingRevision);
        // 恢复的副本没有可观察的执行：running 收敛为 false，由运行侧下一条事实刷新。
        info.set("running", false);
        info.set("executionBlocked", false);
        if (sessionInfo.closed === true) info.set("closed", true);
        else info.delete("closed");
        const closedReason = publicErrorOf(sessionInfo.closedReason);
        if (closedReason) info.set("closedReason", closedReason);
        else info.delete("closedReason");
        const queue = plain(checkpoint.inputQueue);
        if (queue !== undefined && queue !== null) info.set("inputQueue", queue);
        if (checkpoint.legacyIncomplete === true) info.set("legacyIncomplete", true);
        info.delete("activeTurnId");
        info.delete("activeTurnStatus");
        info.delete("activeAssistantEntryId");
        info.delete("pendingRequestId");
        info.delete("activeRequestId");
    });

    // 5. turn 记录：未终态收敛为中断，不伪装完成。
    for (const item of turnsRaw) {
        const rawTurn = object(item) ?? fail("invalid_shape", "turns 条目必须是对象");
        const turnId = string(rawTurn.turnId) ?? fail("invalid_shape", "turns 缺少 turnId");
        const status = string(rawTurn.status) ?? "completed";
        const converged = !isTerminal(status);
        const turn = new Y.Map<unknown>();
        turn.set("turnId", turnId);
        const requestId = string(rawTurn.requestId);
        if (requestId) turn.set("requestId", requestId);
        turn.set("status", converged ? "error" : status);
        turn.set("startedAt", timeOf(rawTurn.startedAt));
        turn.set("completedAt", timeOf(rawTurn.completedAt));
        turn.set("error", converged ? publicError("execution_interrupted") : publicErrorOf(rawTurn.error));
        const stopReason = string(rawTurn.stopReason);
        if (stopReason && !converged) turn.set("stopReason", stopReason);
        turn.set("interactionStatus", "none");
        model.turns().set(turnId, turn);
    }

    // 6. interactions：pending 失效；回答正文与 resolver 从不进入 checkpoint。
    for (const item of interactionsRaw) {
        const rawInteraction = object(item) ?? fail("invalid_shape", "interactions 条目必须是对象");
        const id = string(rawInteraction.id) ?? fail("invalid_shape", "interactions 缺少 id");
        const kind = rawInteraction.kind === "permission" || rawInteraction.kind === "elicitation" ? rawInteraction.kind
            : fail("invalid_shape", `Interaction ${id} 的 kind 非法`);
        const sessionId = string(rawInteraction.sessionId) ?? fail("invalid_shape", `Interaction ${id} 缺少 sessionId`);
        const record = new Y.Map<unknown>();
        record.set("id", id);
        record.set("kind", kind);
        record.set("sessionId", sessionId);
        record.set("status", rawInteraction.status === "resolved" ? "resolved" : "expired");
        record.set("createdAt", timeOf(rawInteraction.createdAt));
        record.set("resolvedAt", timeOf(rawInteraction.resolvedAt));
        for (const key of ["turnId", "toolCallId", "title", "message", "outcome"] as const) {
            const value = string(rawInteraction[key]);
            if (value) record.set(key, value);
        }
        if (Array.isArray(rawInteraction.options)) record.set("options", rawInteraction.options.flatMap((rawOption) => {
            const option = object(rawOption);
            const optionId = string(option?.optionId);
            return optionId ? [{ optionId, name: string(option?.name) ?? optionId }] : [];
        }));
        if (Array.isArray(rawInteraction.fields)) record.set("fields", rawInteraction.fields.flatMap((rawField) => {
            const field = object(rawField);
            const key = string(field?.key);
            return key ? [{ key, title: string(field?.title) ?? key, description: string(field?.description) ?? "" }] : [];
        }));
        model.interactions().set(id, record);
    }
    for (const [, turn] of model.turns()) {
        turn.set("interactionStatus", turnInteractionStatusFor(model.interactions(), String(turn.get("turnId"))));
    }

    // 7. 任务不可观测、队列待刷新（队列仅为展示材料，由应用重新 seed）。
    model.session.transact(() => {
        const tasks = model.tasks();
        const order = model.sessionRoot().get("taskOrder") as Y.Array<string>;
        for (const item of tasksRaw) {
            const rawTask = object(item) ?? fail("invalid_shape", "tasks 条目必须是对象");
            const taskId = string(rawTask.taskId) ?? fail("invalid_shape", "tasks 缺少 taskId");
            if (tasks.has(taskId)) fail("invalid_shape", `taskId ${taskId} 重复`);
            const task = new Y.Map<unknown>();
            task.set("taskId", taskId);
            task.set("kind", string(rawTask.kind) ?? "background");
            task.set("title", string(rawTask.title) ?? "Task");
            const status = string(rawTask.status) ?? "running";
            // 任务在重启后不可观测：非终态标记 unobserved，已有标记不重复加前缀。
            task.set("status", TASK_TERMINAL.has(status) || status.startsWith("unobserved:") ? status : `unobserved:${status}`);
            for (const key of ["summary", "taskSubtype", "startedAt", "completedAt", "updatedAt"] as const) {
                const value = string(rawTask[key]);
                if (value) task.set(key, value);
            }
            const duration = rawTask.durationMs;
            if (typeof duration === "number" && Number.isFinite(duration)) task.set("durationMs", duration);
            if (typeof rawTask.isBackground === "boolean") task.set("isBackground", rawTask.isBackground);
            tasks.set(taskId, task);
            order.push([taskId]);
        }
        const plans = model.sessionRoot().get("plansByTurn") as Y.Map<unknown>;
        for (const [turnId, value] of Object.entries(object(checkpoint.plansByTurn) ?? {})) plans.set(turnId, plain(value));
    });

    // 8. 未完成的聊天对象收敛：assistant 归 error/execution_interrupted，工具归 cancelled。
    model.chat.transact(() => {
        for (const [, entry] of model.entries()) {
            const status = entry.get("status");
            if (isTerminal(status)) continue;
            if (entry.get("role") === "user") entry.set("status", "completed");
            else {
                entry.set("status", "error");
                entry.set("error", publicError("execution_interrupted"));
            }
        }
        for (const [, tool] of model.tools()) {
            if (!isTerminal(tool.get("status"))) tool.set("status", "cancelled");
        }
    });
}
