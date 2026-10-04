import * as Y from "yjs";
import { DocModel } from "./doc-model";
import { TurnMachine } from "./turn-machine";
import { decodeMessagesJson, object, string, toolContentText, type RecordValue } from "./protocol";

/** Live chat aggregation and canonical transcript replacement share entry identities. */
export class ChatProjection {
    private replayEpoch = 0;

    constructor(private readonly docs: DocModel, private readonly turns: TurnMachine) {}

    /** The mailbox delivers real user input without a user_message_chunk notification. */
    acceptDeliveredUserInput(inputId: string, text: string): boolean {
        if (!inputId || !text || this.docs.entries().has(`turn:${inputId}:user`)) return false;
        this.docs.transactBoth(() => this.turns.start(text, inputId));
        return true;
    }

    accept(update: RecordValue, params: RecordValue): boolean {
        const kind = string(update.sessionUpdate);
        if (!kind) return false;
        const sourceAgentId = string(object(object(params._meta)?.peri)?.sourceAgentId);
        if (sourceAgentId && ["agent_message_chunk", "agent_thought_chunk", "tool_call", "tool_call_update"].includes(kind)) return false;
        if (kind === "user_message_chunk") return this.acceptUser(update);
        if (kind === "agent_message_chunk" || kind === "agent_thought_chunk") return this.acceptAssistant(update, kind);
        if (kind === "tool_call" || kind === "tool_call_update") return this.acceptTool(update);
        return false;
    }

    private replayText(entry: Y.Map<unknown>, type: "text" | "reasoning", value: string): void {
        if (entry.get("replayEpoch") !== this.replayEpoch) {
            entry.set("blockOrder", new Y.Array<string>());
            entry.set("blocks", new Y.Map<Y.Map<unknown>>());
            entry.set("replayEpoch", this.replayEpoch);
        }
        this.docs.appendText(entry, type, value);
    }

    private acceptUser(update: RecordValue): boolean {
        const content = object(update.content);
        const text = typeof content?.text === "string" ? content.text : null;
        if (text === null) return false;
        const replayId = string(update.messageId);
        const replay = object(update._meta)?.periReplay === true;
        const existing = replayId ? this.docs.entries().get(`turn:${replayId}:user`) : undefined;
        if (existing) {
            if (!text) return false;
            if (replay && existing.get("replayMessageId") !== replayId) this.replayEpoch++;
            this.docs.chat.transact(() => {
                if (replay) this.replayText(existing, "text", text);
                else this.docs.appendText(existing, "text", text);
                if (replay) existing.set("replayMessageId", replayId);
            });
            return true;
        }
        if (replay) this.replayEpoch++;
        this.docs.transactBoth(() => this.turns.start(text, replayId ?? undefined, replay));
        return true;
    }

    private acceptAssistant(update: RecordValue, kind: "agent_message_chunk" | "agent_thought_chunk"): boolean {
        const content = object(update.content);
        const text = typeof content?.text === "string" ? content.text : null;
        if (!text) return false;
        const replay = object(update._meta)?.periReplay === true;
        const messageId = string(update.messageId);
        // A terminal turn can only be followed by a user turn. Late chunks must not create a ghost turn.
        if (!this.turns.activeTurn() || !this.turns.writable()) return false;
        this.docs.transactBoth(() => {
            const turnId = this.turns.activeTurn()!;
            const info = this.docs.info();
            let entryId = string(info.get("activeAssistantEntryId")) ?? `${turnId}:assistant`;
            let entry = this.docs.ensureEntry(entryId, turnId, "assistant");
            const currentMessageId = string(entry.get("messageId"));
            if (messageId && currentMessageId && currentMessageId !== messageId) {
                entry.set("status", "completed");
                entryId = `${turnId}:assistant:${messageId}`;
                entry = this.docs.ensureEntry(entryId, turnId, "assistant");
                info.set("activeAssistantEntryId", entryId);
            }
            const type = kind === "agent_message_chunk" ? "text" : "reasoning";
            if (replay && messageId && entry.get("messageId") === messageId) this.replayText(entry, type, text);
            else this.docs.appendText(entry, type, text);
            if (messageId && entry.get("messageId") !== messageId) entry.set("messageId", messageId);
            if (replay && messageId) entry.set("replayMessageId", messageId);
            if (replay) entry.set("replayEpoch", this.replayEpoch);
            entry.set("status", "streaming");
            if (info.get("activeTurnStatus") !== "running") info.set("activeTurnStatus", "running");
        });
        return true;
    }

    private acceptTool(update: RecordValue): boolean {
        const id = string(update.toolCallId) ?? string(update.id);
        const turnId = this.turns.activeTurn();
        if (!id || !turnId || !this.turns.writable()) return false;
        const rawStatus = string(update.status);
        const status = rawStatus === "completed" ? "completed"
            : rawStatus === "failed" || rawStatus === "error" ? "error"
            : rawStatus === "cancelled" ? "cancelled"
            : rawStatus === "pending" ? "pending" : "running";
        const existing = this.docs.tools().get(id);
        const oldStatus = existing?.get("status");
        if ((oldStatus === "completed" || oldStatus === "error" || oldStatus === "cancelled") && oldStatus !== status) return false;
        this.docs.chat.transact(() => {
            const tool = this.docs.ensureTool(id, turnId, string(update.title) ?? string(update.name) ?? "Tool");
            const alreadyAttached = [...this.docs.entries().values()].some((entry) =>
                entry.get("turnId") === turnId && (entry.get("blocks") as Y.Map<unknown>).has(`tool:${id}`));
            if (!alreadyAttached) {
                const entryId = string(this.docs.info().get("activeAssistantEntryId")) ?? `${turnId}:assistant`;
                this.docs.addToolBlock(this.docs.ensureEntry(entryId, turnId, "assistant"), id);
            }
            const name = string(update.title) ?? string(update.name);
            if (name) tool.set("name", name);
            const kind = string(update.kind);
            if (kind) tool.set("kind", kind);
            if (update.rawInput !== undefined) tool.set("arguments", update.rawInput);
            if (status === "completed" || status === "error") {
                const contentText = toolContentText(update.content);
                const result: RecordValue = {};
                if (contentText !== null) result.contentText = status === "error" && !contentText.trim()
                    ? "Tool execution failed" : contentText;
                else if (status === "error") result.contentText = "Tool execution failed";
                if (update.rawOutput !== undefined) result.rawOutput = update.rawOutput;
                if (Object.keys(result).length) tool.set("result", result);
            }
            tool.set("status", status);
        });
        return true;
    }

    replaceHistory(raw: unknown, keepTurnOpen: boolean): boolean {
        const messages = decodeMessagesJson(raw);
        if (!messages) return false;
        this.replayEpoch++;
        this.docs.transactBoth(() => {
            this.docs.resetChat();
            let turnId = "history:0";
            let lastAssistantEntryId: string | null = null;
            const completedTools = new Map<string, { failed: boolean; resultText: string }>();
            for (const message of messages) {
                if (message.role === "user") turnId = `history:${message.id}`;
                if (message.role === "tool") {
                    if (message.toolCallId) completedTools.set(message.toolCallId, {
                        failed: message.isError === true, resultText: message.resultText ?? "",
                    });
                    continue;
                }
                const entryId = `message:${message.id}`;
                const entry = this.docs.ensureEntry(entryId, turnId, message.role);
                if (message.role === "assistant") lastAssistantEntryId = entryId;
                entry.set("messageId", message.id);
                entry.set("status", "completed");
                for (const block of message.blocks) {
                    if (block.type === "text" || block.type === "reasoning") {
                        if (block.text) this.docs.appendText(entry, block.type, block.text);
                    } else if (block.type === "tool") {
                        const tool = this.docs.ensureTool(block.id, turnId, block.name);
                        if (block.arguments !== undefined) tool.set("arguments", block.arguments);
                        this.docs.addToolBlock(entry, block.id);
                    }
                }
            }
            for (const [id, tool] of this.docs.tools()) {
                const outcome = completedTools.get(id);
                // A tool_use without its matching tool result is incomplete, even in a canonical snapshot.
                tool.set("status", outcome === undefined ? keepTurnOpen ? "running" : "cancelled" : outcome.failed ? "error" : "completed");
                if (outcome) tool.set("result", { contentText: outcome.failed && !outcome.resultText.trim()
                    ? "Tool execution failed" : outcome.resultText });
            }
            this.turns.resetAfterHistory(turnId, lastAssistantEntryId, keepTurnOpen);
        });
        return true;
    }
}
