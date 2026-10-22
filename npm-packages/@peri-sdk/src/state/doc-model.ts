import * as Y from "yjs";
import { ToolPayloads } from "./tool-payloads";

export type EntryRole = "user" | "assistant";
export type ProjectionClock = () => string;
export const PUBLIC_ERROR_CODES = ["execution_failed", "execution_interrupted", "tool_failed", "payload_too_large"] as const;
export type PublicErrorCode = (typeof PUBLIC_ERROR_CODES)[number];
export type PublicError = { code: PublicErrorCode; message: string };
const PUBLIC_ERRORS: Record<PublicErrorCode, string> = {
    execution_failed: "执行失败",
    execution_interrupted: "执行已中断",
    tool_failed: "工具执行失败",
    payload_too_large: "工具载荷超出会话接纳预算",
};

export function isPublicErrorCode(value: unknown): value is PublicErrorCode {
    return typeof value === "string" && (PUBLIC_ERROR_CODES as readonly string[]).includes(value);
}
/** 文档 schema 版本（P3 已批准升级：chat 3 / session 2）。 */
export const CHAT_SCHEMA_VERSION = 3;
export const SESSION_SCHEMA_VERSION = 2;

/** 公开错误只能来自固定映射；上游诊断正文不进入 Yjs。 */
export function publicError(code: PublicErrorCode): PublicError {
    return { code, message: PUBLIC_ERRORS[code] };
}

export function isTerminal(status: unknown): boolean {
    return status === "completed" || status === "cancelled" || status === "error";
}

/** One owner for the Yjs schema and its low-level chat writes. */
export class DocModel {
    readonly chat = new Y.Doc();
    readonly session = new Y.Doc();
    readonly payloads: ToolPayloads;
    private readonly entriesByTurn = new Map<string, Set<string>>();
    private readonly toolsByTurn = new Map<string, Set<string>>();

    constructor(readonly now: ProjectionClock = () => new Date().toISOString(), payloads?: ToolPayloads) {
        this.payloads = payloads ?? new ToolPayloads();
        this.chat.transact(() => {
            const root = this.chatRoot();
            root.set("schemaVersion", CHAT_SCHEMA_VERSION);
            root.set("entryOrder", new Y.Array<string>());
            root.set("entries", new Y.Map<Y.Map<unknown>>());
            root.set("toolCalls", new Y.Map<Y.Map<unknown>>());
        });
        this.session.transact(() => {
            const root = this.sessionRoot();
            root.set("schemaVersion", SESSION_SCHEMA_VERSION);
            root.set("session", new Y.Map<unknown>());
            root.set("turns", new Y.Map<Y.Map<unknown>>());
            root.set("interactions", new Y.Map<Y.Map<unknown>>());
            root.set("tasks", new Y.Map<Y.Map<unknown>>());
            root.set("taskOrder", new Y.Array<string>());
            root.set("plansByTurn", new Y.Map<unknown>());
        });
    }

    destroy(): void { this.payloads.clear(); this.chat.destroy(); this.session.destroy(); }
    chatRoot(): Y.Map<unknown> { return this.chat.getMap("root"); }
    sessionRoot(): Y.Map<unknown> { return this.session.getMap("root"); }
    info(): Y.Map<unknown> { return this.sessionRoot().get("session") as Y.Map<unknown>; }
    entries(): Y.Map<Y.Map<unknown>> { return this.chatRoot().get("entries") as Y.Map<Y.Map<unknown>>; }
    tools(): Y.Map<Y.Map<unknown>> { return this.chatRoot().get("toolCalls") as Y.Map<Y.Map<unknown>>; }
    tasks(): Y.Map<Y.Map<unknown>> { return this.sessionRoot().get("tasks") as Y.Map<Y.Map<unknown>>; }
    turns(): Y.Map<Y.Map<unknown>> { return this.sessionRoot().get("turns") as Y.Map<Y.Map<unknown>>; }
    interactions(): Y.Map<Y.Map<unknown>> { return this.sessionRoot().get("interactions") as Y.Map<Y.Map<unknown>>; }
    turnEntries(turnId: string): Iterable<string> { return this.entriesByTurn.get(turnId) ?? []; }
    turnTools(turnId: string): Iterable<string> { return this.toolsByTurn.get(turnId) ?? []; }

    private index(index: Map<string, Set<string>>, turnId: string, id: string): void {
        let ids = index.get(turnId);
        if (!ids) { ids = new Set(); index.set(turnId, ids); }
        ids.add(id);
    }

    /** Mutations to both documents are coordinated here; each Y.Doc publishes its own transaction. */
    transactBoth(action: () => void): void {
        this.chat.transact(() => this.session.transact(action));
    }

    resetChat(): void {
        this.entriesByTurn.clear();
        this.toolsByTurn.clear();
        this.payloads.clear();
        const root = this.chatRoot();
        const order = root.get("entryOrder") as Y.Array<string>;
        order.delete(0, order.length);
        (root.get("entries") as Y.Map<Y.Map<unknown>>).clear();
        (root.get("toolCalls") as Y.Map<Y.Map<unknown>>).clear();
        (this.sessionRoot().get("plansByTurn") as Y.Map<unknown>).clear();
        this.info().delete("plan");
    }

    ensureEntry(id: string, turnId: string, role: EntryRole, historical = false): Y.Map<unknown> {
        const existing = this.entries().get(id);
        if (existing) return existing;
        const createdAt = historical ? null : this.now();
        const entry = new Y.Map<unknown>();
        entry.set("entryId", id);
        entry.set("id", id);
        entry.set("kind", "message");
        entry.set("createdAt", createdAt);
        entry.set("createdAtSource", historical ? "unknown" : "observed");
        entry.set("completedAt", role === "user" ? createdAt : null);
        entry.set("error", null);
        entry.set("turnId", turnId);
        entry.set("role", role);
        entry.set("status", role === "user" ? "completed" : "pending");
        entry.set("blockOrder", new Y.Array<string>());
        entry.set("blocks", new Y.Map<Y.Map<unknown>>());
        this.entries().set(id, entry);
        this.index(this.entriesByTurn, turnId, id);
        (this.chatRoot().get("entryOrder") as Y.Array<string>).push([id]);
        return entry;
    }

    appendText(entry: Y.Map<unknown>, type: "text" | "reasoning", value: string): void {
        const order = entry.get("blockOrder") as Y.Array<string>;
        const blocks = entry.get("blocks") as Y.Map<Y.Map<unknown>>;
        const lastId = order.length ? order.get(order.length - 1) : null;
        let block = lastId ? blocks.get(lastId) : undefined;
        if (block?.get("type") !== type) {
            const id = `${type}:${order.length}`;
            block = new Y.Map<unknown>();
            block.set("blockId", id);
            block.set("type", type);
            block.set("text", new Y.Text());
            if (type === "reasoning") block.set("visibility", "summary");
            blocks.set(id, block);
            order.push([id]);
        }
        const text = block.get("text") as Y.Text;
        text.insert(text.length, value);
    }

    addToolBlock(entry: Y.Map<unknown>, id: string): void {
        const blockId = `tool:${id}`;
        const blocks = entry.get("blocks") as Y.Map<Y.Map<unknown>>;
        if (blocks.has(blockId)) return;
        const block = new Y.Map<unknown>();
        block.set("blockId", blockId);
        block.set("type", "tool_call");
        block.set("toolCallId", id);
        blocks.set(blockId, block);
        (entry.get("blockOrder") as Y.Array<string>).push([blockId]);
        this.tools().get(id)?.set("entryId", entry.get("entryId"));
    }

    // 首次终态才写时间和公开错误；历史未知时间保持 null。
    finish(target: Y.Map<unknown>, status: "completed" | "cancelled" | "error", errorCode: PublicErrorCode = "execution_failed"): void {
        if (isTerminal(target.get("status"))) return;
        target.set("status", status);
        target.set("completedAt", target.get("createdAt") === null ? null : this.now());
        if (status === "error") target.set("error", { code: errorCode, message: PUBLIC_ERRORS[errorCode] } satisfies PublicError);
    }

    /**
     * 工具实例身份全局唯一：已存在即复用（保留创建时的 `turnId`），不判断调用方 turn。
     *
     * 「旧 turn 的工具 ID 不得被新 turn 重绑」由实时路径（`ChatProjection.acceptToolCall`）在
     * 写入前显式判定；历史页合并（`HistoryMerge.mergeTool`）跨页更新同一调用实例是正常输入。
     */
    ensureTool(id: string, turnId: string, name: string, historical = false): Y.Map<unknown> {
        const existing = this.tools().get(id);
        if (existing) return existing;
        const tool = new Y.Map<unknown>();
        tool.set("createdAt", historical ? null : this.now());
        tool.set("completedAt", null);
        tool.set("error", null);
        tool.set("status", "pending");
        tool.set("toolCallId", id);
        tool.set("turnId", turnId);
        tool.set("name", name);
        this.tools().set(id, tool);
        this.index(this.toolsByTurn, turnId, id);
        return tool;
    }
}
