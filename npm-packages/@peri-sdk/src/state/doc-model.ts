import * as Y from "yjs";
import { ToolPayloads } from "./tool-payloads";

export type EntryRole = "user" | "assistant";

/** One owner for the Yjs schema and its low-level chat writes. */
export class DocModel {
    readonly chat = new Y.Doc();
    readonly session = new Y.Doc();
    readonly payloads = new ToolPayloads();
    private readonly entriesByTurn = new Map<string, Set<string>>();
    private readonly toolsByTurn = new Map<string, Set<string>>();

    constructor() {
        this.chat.transact(() => {
            const root = this.chatRoot();
            root.set("schemaVersion", 2);
            root.set("entryOrder", new Y.Array<string>());
            root.set("entries", new Y.Map<Y.Map<unknown>>());
            root.set("toolCalls", new Y.Map<Y.Map<unknown>>());
        });
        this.session.transact(() => {
            const root = this.sessionRoot();
            root.set("schemaVersion", 1);
            root.set("session", new Y.Map<unknown>());
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

    ensureEntry(id: string, turnId: string, role: EntryRole): Y.Map<unknown> {
        const existing = this.entries().get(id);
        if (existing) return existing;
        const entry = new Y.Map<unknown>();
        entry.set("entryId", id);
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

    ensureTool(id: string, turnId: string, name: string): Y.Map<unknown> {
        const existing = this.tools().get(id);
        if (existing) return existing;
        const tool = new Y.Map<unknown>();
        tool.set("toolCallId", id);
        tool.set("turnId", turnId);
        tool.set("name", name);
        this.tools().set(id, tool);
        this.index(this.toolsByTurn, turnId, id);
        return tool;
    }
}
