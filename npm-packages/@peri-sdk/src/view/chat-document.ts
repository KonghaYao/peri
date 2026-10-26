import type * as Y from "yjs";
import type { ChatBindingTarget } from "../chat/types";
import type { ChatFact, ChatFactEnvelope, InteractionFactRecord } from "../chat/facts";
import type { JsonRpcNotification } from "../transport/types";
import { ChatProjection } from "../state/chat-projection";
import { applyCheckpoint, exportCheckpoint, type DisplayCheckpoint } from "../state/checkpoint";
import { DocModel, type ProjectionClock, type PublicError } from "../state/doc-model";
import { HistoryMerge, historyEntryRefOf, type HistoryPageItem } from "../state/history-merge";
import { InputQueueProjection } from "../state/input-queue-projection";
import { InteractionProjection, type FormField } from "../state/interaction-projection";
import { decodeAgentEvent, object, string } from "../state/protocol";
import { SessionProjection } from "../state/session-projection";
import { TaskProjection } from "../state/task-projection";
import { TurnMachine, type TurnExitStatus } from "../state/turn-machine";

type ChatDocumentOptions = {
    now?: ProjectionClock;
    /** 当前绑定代次；文档只在单一代次内累积事实。 */
    binding?: ChatBindingTarget;
};

/**
 * View 侧的纯展示文档（一对 Y.Doc）。
 *
 * 只消费运行侧事实与归一化通知：不持有 resolver、历史 cursor、任务补读 requester、
 * 载荷全文或执行恢复状态；也不发起会改变运行侧的动作。
 */
export class ChatDocument {
    readonly chat: Y.Doc;
    readonly session: Y.Doc;
    private readonly model: DocModel;
    private readonly turns: TurnMachine;
    private readonly chatProjection: ChatProjection;
    private readonly historyMerge: HistoryMerge;
    private readonly sessionProjection: SessionProjection;
    private readonly taskProjection: TaskProjection;
    private readonly inputQueue: InputQueueProjection;
    private readonly interaction: InteractionProjection;
    private readonly seenEvents = new Set<string>();
    private destroyed = false;
    private closedState = false;
    private target: ChatBindingTarget | null = null;
    private lastSequence = -1;
    private cancelToken: { turnId: string; previous: "accepting" | "running" } | null = null;

    constructor(options: ChatDocumentOptions = {}) {
        this.model = new DocModel(options.now ?? (() => new Date().toISOString()));
        this.turns = new TurnMachine(this.model);
        this.chatProjection = new ChatProjection(this.model, this.turns);
        this.historyMerge = new HistoryMerge(this.model);
        this.sessionProjection = new SessionProjection(this.model, this.turns);
        this.taskProjection = new TaskProjection(this.model);
        this.inputQueue = new InputQueueProjection(this.model);
        this.interaction = new InteractionProjection(this.model);
        this.chat = this.model.chat;
        this.session = this.model.session;
        this.model.session.transact(() => {
            const info = this.model.info();
            info.set("running", false);
            info.set("executionBlocked", false);
        });
        if (options.binding) this.bind(options.binding);
    }

    /** 从展示 checkpoint 构造全新文档；不作用于任何活跃 doc。 */
    static fromCheckpoint(checkpoint: unknown, options: ChatDocumentOptions = {}): ChatDocument {
        const document = new ChatDocument(options);
        applyCheckpoint(checkpoint, document.model);
        return document;
    }

    get closed(): boolean { return this.closedState; }
    get binding(): ChatBindingTarget | null { return this.target; }

    bind(target: ChatBindingTarget): void {
        if (this.destroyed) return;
        this.target = { ...target };
        this.lastSequence = -1;
        this.model.session.transact(() => {
            const info = this.model.info();
            info.set("sessionId", target.sessionId);
            info.set("bindingRevision", target.bindingRevision);
        });
    }

    /** 事实代次与序号必须属于当前绑定；迟到/重复事实不写入新代次。 */
    applyEnvelope(envelope: ChatFactEnvelope): boolean {
        if (this.destroyed) return false;
        if (this.target && envelope.bindingRevision !== this.target.bindingRevision) return false;
        if (envelope.sequence <= this.lastSequence) return false;
        this.lastSequence = envelope.sequence;
        return this.applyFact(envelope.fact);
    }

    /** onBinding 的同步重放：整批一个事务，观察者看不到半份首屏。 */
    applyFacts(envelopes: Iterable<ChatFactEnvelope>): void {
        if (this.destroyed) return;
        this.model.transactBoth(() => {
            for (const envelope of envelopes) this.applyEnvelope(envelope);
        });
    }

    applyFact(fact: ChatFact): boolean {
        if (this.destroyed) return false;
        // closed 是展示终态：之后的迟到事实不得把会话重新打开。
        if (this.closedState) return fact.kind === "closed" ? this.applyClosed(fact.reason) : false;
        switch (fact.kind) {
            case "seed-config": return this.seedConfig(fact.response);
            case "history-batch": return this.acceptHistoryBatch(fact.notifications);
            case "notice": this.accept(fact.notification); return true;
            case "history-page": return this.mergeHistoryItems(fact.items) > 0;
            case "input-queue": return this.inputQueue.seed(fact.snapshot);
            case "input-delivered": return this.acceptDeliveredUserInput(fact.inputId, fact.text);
            case "turn-complete": return this.completeTurn(fact.status, fact.stopReason);
            case "turn-cancel": return this.applyTurnCancel(fact.restore);
            case "interaction-began": return this.beginInteraction(fact.record);
            case "interaction-settled": return this.interaction.settle(fact.id, fact.status, fact.outcome);
            case "interactions-expired": return this.interaction.expireAll();
            case "task-snapshot": return this.taskProjection.acceptBackground("bg-task-snapshot", object(fact.snapshot));
            case "run-state": return this.applyRunState(fact.running, fact.executionBlocked);
            case "closed": return this.applyClosed(fact.reason);
        }
    }

    seedConfig(response: unknown): boolean {
        if (this.destroyed || !object(response)) return false;
        this.sessionProjection.seedConfig(response);
        return true;
    }

    /** 运行侧已校验的队列快照；View 不据此推断投递或执行资格。 */
    seedInputQueue(snapshot: unknown): boolean {
        if (this.destroyed) return false;
        return this.inputQueue.seed(snapshot);
    }

    acceptDeliveredUserInput(inputId: string, text: string): boolean {
        if (this.destroyed) return false;
        return this.chatProjection.acceptDeliveredUserInput(inputId, text);
    }

    acceptBatch(notifications: Iterable<JsonRpcNotification>): void {
        if (this.destroyed) return;
        this.model.transactBoth(() => { for (const notification of notifications) this.accept(notification); });
    }

    acceptHistoryBatch(notifications: Iterable<JsonRpcNotification>): boolean {
        if (this.destroyed) return false;
        this.acceptBatch(notifications);
        return true;
    }

    /** 只合并已由运行侧校验过的历史条目；窗口与 cursor 不进入 View。 */
    mergeHistoryItems(items: Iterable<HistoryPageItem>): number {
        if (this.destroyed) return 0;
        let applied = 0;
        this.model.transactBoth(() => {
            for (const item of items) if (this.historyMerge.applyEntry(item, item.subItems)) applied++;
        });
        return applied;
    }

    completeTurn(status: TurnExitStatus = "completed", stopReason?: string): boolean {
        if (this.destroyed) return false;
        return this.turns.exit(status, stopReason);
    }

    private applyTurnCancel(restore: boolean): boolean {
        if (this.destroyed) return false;
        if (!restore) {
            const token = this.turns.requestCancel();
            if (!token) return false;
            this.cancelToken = token;
            return true;
        }
        // 取消失败：展示退回请求取消前的可写状态；具体 turn/tool 终态仍由后续事实决定。
        const token = this.cancelToken;
        if (!token) return false;
        if (this.model.info().get("activeTurnStatus") !== "cancelling") {
            this.cancelToken = null;
            return false;
        }
        this.turns.restoreCancel(token);
        this.cancelToken = null;
        return true;
    }

    private beginInteraction(record: InteractionFactRecord): boolean {
        return this.interaction.begin({
            ...record,
            fields: record.fields as FormField[] | undefined,
            questions: record.questions,
        });
    }

    applyRunState(running: boolean, executionBlocked: boolean): boolean {
        if (this.destroyed) return false;
        return this.sessionProjection.acceptRunState(running, executionBlocked);
    }

    applyClosed(reason?: PublicError): boolean {
        if (this.destroyed) return false;
        this.closedState = true;
        this.model.session.transact(() => {
            const info = this.model.info();
            info.set("running", false);
            info.set("closed", true);
            if (reason) info.set("closedReason", reason);
            else info.delete("closedReason");
        });
        return true;
    }

    /** 单条 ACP/Peri 通知的展示路由；运行侧只投递已 admit 的通知。 */
    accept(notification: JsonRpcNotification): void {
        if (this.destroyed || !notification || typeof notification !== "object") return;
        const params = object(notification.params);
        if (!params) return;
        const eventId = string(object(object(params._meta)?.peri)?.eventId);
        if (eventId && this.seenEvents.has(eventId)) return;

        let accepted = false;
        if (notification.method === "session/update") {
            const update = object(params.update);
            if (!update) return;
            const entryRef = historyEntryRefOf(update);
            if (entryRef) {
                accepted = this.historyMerge.applyEntry(entryRef, [{ position: null, update }]);
            } else {
                accepted = this.chatProjection.accept(update, params)
                    || this.sessionProjection.acceptUpdate(update, params);
            }
        } else if (notification.method === "peri/agent_event") {
            const event = decodeAgentEvent(params.event_json);
            if (!event) return;
            accepted = this.acceptAgentEvent(event.type, event.value);
        } else if (notification.method === "peri/agent_event_done") {
            const requestId = string(params.requestId);
            if (!this.turns.acceptsDone(requestId)) return;
            const reason = string(params.stopReason);
            const status: TurnExitStatus = reason === "cancelled" ? "cancelled"
                : reason === "max_tokens" || reason === "max_turn_requests" || reason === "error" ? "error" : "completed";
            accepted = this.completeTurn(status, reason ?? undefined);
        } else if (notification.method === "peri/unstable_event") {
            accepted = this.taskProjection.acceptBackground(string(params.event), object(params.data));
        }
        if (accepted && eventId) this.seenEvents.add(eventId);
    }

    private acceptAgentEvent(type: string, value: Record<string, unknown> | null): boolean {
        if (type === "user_input_run_started" && value) {
            const requestId = string(value.request_id);
            if (!requestId) return false;
            this.model.info().set("pendingRequestId", requestId);
            return true;
        }
        if (type === "compact_completed" && value) {
            return this.sessionProjection.acceptAgentEvent(type, value);
        }
        if (type === "rewind_completed" && value) {
            // 展示范围变了：重建历史窗口；执行身份与交互终态由运行侧后续事实给出。
            const refreshed = this.chatProjection.replaceHistory(value.messages_json, "preserve-execution");
            this.historyMerge.reset();
            const observed = this.sessionProjection.acceptAgentEvent(type, value);
            return refreshed || observed;
        }
        return this.inputQueue.acceptAgentEvent(type, value)
            || this.sessionProjection.acceptAgentEvent(type, value)
            || this.taskProjection.acceptSubagent(type, value);
    }

    exportCheckpoint(): DisplayCheckpoint { return exportCheckpoint(this.model); }

    destroy(): void {
        if (this.destroyed) return;
        this.destroyed = true;
        this.closedState = true;
        this.seenEvents.clear();
        this.historyMerge.reset();
        this.model.destroy();
    }
}
