import * as Y from "yjs";
import { DocModel, isTerminal, publicError } from "./doc-model";
import { TurnMachine } from "./turn-machine";
import { ToolPayloadCapacityError } from "./tool-payloads";
import { decodeMessagesJson, object, string, toolContentText, type RecordValue } from "./protocol";

/**
 * 规范历史替换（`replaceHistory`）的两种语义。Compact 不再走这条路（REQ-C02）：
 * 只有 rewind 这种「服务端真的删掉了尾部行、范围变了」的调用才会重建窗口。
 */
export type HistoryReplaceMode =
    /** 默认：历史是唯一事实源，实时执行状态随之清空（首窗安装等冷路径）。 */
    | "reset-execution"
    /** rewind：窗口失效并刷新，但当前 turn／request 与未决交互保留（REQ-C02）。 */
    | "preserve-execution";

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
        if (!content || !text || (content.type !== undefined && content.type !== "text")) return false;
        if (kind === "agent_thought_chunk" && [content.visibility, update.visibility, object(update._meta)?.visibility]
            .some((visibility) => visibility !== undefined && visibility !== "summary")) return false;
        const replay = object(update._meta)?.periReplay === true;
        const messageId = string(update.messageId);
        // A terminal turn can only be followed by a user turn. Late chunks must not create a ghost turn.
        if (!this.turns.activeTurn() || !this.turns.writable()) return false;
        if (messageId && !replay) {
            for (const id of this.docs.turnEntries(this.turns.activeTurn()!)) {
                const previous = this.docs.entries().get(id)!;
                if (previous.get("messageId") === messageId && isTerminal(previous.get("status"))) return false;
            }
        }
        this.docs.transactBoth(() => {
            const turnId = this.turns.activeTurn()!;
            const info = this.docs.info();
            let entryId = string(info.get("activeAssistantEntryId")) ?? `${turnId}:assistant`;
            let entry = this.docs.ensureEntry(entryId, turnId, "assistant", replay);
            const currentMessageId = string(entry.get("messageId"));
            if (messageId && currentMessageId && currentMessageId !== messageId) {
                this.docs.finish(entry, "completed");
                entryId = `${turnId}:assistant:${messageId}`;
                entry = this.docs.ensureEntry(entryId, turnId, "assistant", replay);
                info.set("activeAssistantEntryId", entryId);
            }
            const type = kind === "agent_message_chunk" ? "text" : "reasoning";
            if (replay && messageId && entry.get("messageId") === messageId) this.replayText(entry, type, text);
            else this.docs.appendText(entry, type, text);
            if (messageId && entry.get("messageId") !== messageId) entry.set("messageId", messageId);
            if (replay && messageId) entry.set("replayMessageId", messageId);
            if (replay) entry.set("replayEpoch", this.replayEpoch);
            if (entry.get("status") !== "streaming") entry.set("status", "streaming");
            this.turns.markRunning();
        });
        return true;
    }

    private acceptTool(update: RecordValue): boolean {
        const id = string(update.toolCallId) ?? string(update.id);
        const turnId = this.turns.activeTurn();
        if (!id || !turnId || !this.turns.writable()) return false;
        const replay = object(update._meta)?.periReplay === true;
        const existing = this.docs.tools().get(id);
        if (existing && existing.get("turnId") !== turnId) {
            if (update.sessionUpdate === "tool_call") throw new Error("tool_id_reused");
            return false;
        }
        // 无所属调用的补丁不能构造工具；新调用显式接纳，缺省状态保持 pending。
        if (!existing && update.sessionUpdate === "tool_call_update") return false;
        const oldStatus = existing?.get("status");
        const rawStatus = string(update.status);
        const status = rawStatus === null ? String(oldStatus ?? "pending")
            : rawStatus === "completed" ? "completed"
            : rawStatus === "failed" || rawStatus === "error" ? "error"
            : rawStatus === "cancelled" ? "cancelled"
            : rawStatus === "pending" || rawStatus === "awaiting_permission" ? rawStatus
            : rawStatus === "in_progress" ? "running" : null;
        if (!status || (isTerminal(oldStatus) && oldStatus !== status)) return false;
        let payloadRejected = false;
        const writePayload = (tool: Y.Map<unknown>, field: "arguments" | "result", value: unknown): void => {
            try { this.docs.payloads.set(tool, field, value); }
            catch (error) {
                if (!(error instanceof ToolPayloadCapacityError)) throw error;
                // 超出接纳预算：不写入该字段，前一个版本保持可读，并以公开容量错误报告。
                payloadRejected = true;
            }
        };
        this.docs.chat.transact(() => {
            const tool = this.docs.ensureTool(id, turnId, string(update.title) ?? string(update.name) ?? "Tool", replay);
            const ownerId = string(tool.get("entryId"));
            const owner = ownerId ? this.docs.entries().get(ownerId) : undefined;
            const alreadyAttached = owner?.get("turnId") === turnId &&
                (owner.get("blocks") as Y.Map<unknown>).has(`tool:${id}`);
            if (!alreadyAttached) {
                const entryId = string(this.docs.info().get("activeAssistantEntryId")) ?? `${turnId}:assistant`;
                this.docs.addToolBlock(this.docs.ensureEntry(entryId, turnId, "assistant", replay), id);
            }
            const name = string(update.title) ?? string(update.name);
            if (name && tool.get("name") !== name) tool.set("name", name);
            const kind = string(update.kind);
            if (kind && tool.get("kind") !== kind) tool.set("kind", kind);
            if (update.rawInput !== undefined) writePayload(tool, "arguments", update.rawInput);
            if (status === "completed" || status === "error") {
                const contentText = toolContentText(update.content);
                const result: RecordValue = {};
                if (contentText !== null) result.contentText = status === "error" && !contentText.trim()
                    ? "Tool execution failed" : contentText;
                else if (status === "error") result.contentText = "Tool execution failed";
                if (update.rawOutput !== undefined) result.rawOutput = update.rawOutput;
                if (Object.keys(result).length) writePayload(tool, "result", result);
            }
            if (status === "completed" || status === "error" || status === "cancelled") this.docs.finish(tool, status, "tool_failed");
            else if (tool.get("status") !== status) tool.set("status", status);
            if (payloadRejected && object(tool.get("error"))?.code !== "tool_failed") {
                tool.set("error", publicError("payload_too_large"));
            }
        });
        return true;
    }

    /**
     * 用一份规范消息列表重建聊天窗口。
     *
     * `preserve-execution`（rewind）：实时执行身份与未决交互是独立于历史窗口的事实，
     * 重建后原样恢复——历史范围变化不等于当前 turn 结束（REQ-C02）。
     */
    replaceHistory(raw: unknown, mode: HistoryReplaceMode = "reset-execution"): boolean {
        const messages = decodeMessagesJson(raw);
        if (!messages) return false;
        const preserveExecution = mode === "preserve-execution";
        this.replayEpoch++;
        this.docs.transactBoth(() => {
            // 实时执行身份在重建前快照：resetChat 只清历史窗口，不继承 turn 生命周期。
            const info = this.docs.info();
            const live = preserveExecution
                ? {
                    activeTurnId: info.get("activeTurnId"),
                    activeTurnStatus: info.get("activeTurnStatus"),
                    activeAssistantEntryId: info.get("activeAssistantEntryId"),
                    activeRequestId: info.get("activeRequestId"),
                    pendingRequestId: info.get("pendingRequestId"),
                }
                : null;
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
                const entry = this.docs.ensureEntry(entryId, turnId, message.role, true);
                if (message.role === "assistant") lastAssistantEntryId = entryId;
                entry.set("messageId", message.id);
                this.docs.finish(entry, "completed");
                for (const block of message.blocks) {
                    if (block.type === "text" || block.type === "reasoning") {
                        if (block.text) this.docs.appendText(entry, block.type, block.text);
                    } else if (block.type === "tool") {
                        const tool = this.docs.ensureTool(block.id, turnId, block.name, true);
                        if (block.arguments !== undefined) this.docs.payloads.set(tool, "arguments", block.arguments);
                        this.docs.addToolBlock(entry, block.id);
                    }
                }
            }
            for (const [id, tool] of this.docs.tools()) {
                const outcome = completedTools.get(id);
                // A tool_use without its matching tool result is incomplete, even in a canonical snapshot.
                tool.set("status", outcome === undefined ? "cancelled" : outcome.failed ? "error" : "completed");
                if (outcome) this.docs.payloads.set(tool, "result", { contentText: outcome.failed && !outcome.resultText.trim()
                    ? "Tool execution failed" : outcome.resultText });
            }
            if (live) {
                // 逐字段恢复：缺失即为缺失（原值为 undefined 时不写回 null 覆盖）。
                for (const [key, value] of Object.entries(live)) {
                    if (value === undefined) info.delete(key);
                    else info.set(key, value);
                }
            } else {
                this.turns.resetAfterHistory(turnId, lastAssistantEntryId, false);
            }
        });
        return true;
    }
}
