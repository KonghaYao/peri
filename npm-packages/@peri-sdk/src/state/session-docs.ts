import type * as Y from "yjs";
import type { JsonRpcNotification } from "../transport/types";
import type { InteractionCallbacks } from "../agent/types";
import { ChatProjection } from "./chat-projection";
import { applyCheckpoint, exportCheckpoint, type DisplayCheckpoint } from "./checkpoint";
import { DocModel, type ProjectionClock } from "./doc-model";
import { HistoryMerge, decodeHistoryPageItems, historyEntryRefOf } from "./history-merge";
import { InputQueueProjection } from "./input-queue-projection";
import { InteractionProjection } from "./interaction-projection";
import { decodeAgentEvent, object, string } from "./protocol";
import { SessionProjection } from "./session-projection";
import { TaskProjection } from "./task-projection";
import { ToolPayloads, type ToolPayloadBudgets } from "./tool-payloads";
import { TurnMachine, type TurnExitStatus } from "./turn-machine";

export type SessionDocsOptions = { now?: ProjectionClock; payloadBudgets?: ToolPayloadBudgets };

/**
 * 已加载历史窗口的浏览身份与翻页进度（REQ-C01、REQ-H02）。
 *
 * `snapshotToken` 是**同一次浏览**的范围身份：页与页、页与首屏只在同一个 token 内可比。
 * rewind／重新初始 load 之后旧 token 一律作废，旧范围响应整页丢弃。
 */
type HistoryWindow = { snapshotToken: string; nextCursor: string | null };

/** 一页响应的合并结果；`stale` = 会话或浏览快照已变（验收 A11），`malformed` = 形状不合契约。 */
export type HistoryPageMerge = {
    applied: number;
    nextCursor: string | null;
    status: "merged" | "stale" | "malformed";
};

/** One Agent's live ACP projection. Session.start filters sessionId before accept. */
export class SessionDocs {
    private readonly model: DocModel;
    private readonly turns: TurnMachine;
    private readonly chatProjection: ChatProjection;
    private readonly historyMerge: HistoryMerge;
    private readonly sessionProjection: SessionProjection;
    private readonly taskProjection: TaskProjection;
    private readonly inputQueue: InputQueueProjection;
    private readonly interaction: InteractionProjection;
    private readonly seenEvents = new Set<string>();
    private historyWindow: HistoryWindow | null = null;
    /** 窗口被 rewind 作废：游标没了，但「范围失效」与「从未协商」是两回事（验收 A11）。 */
    private historyInvalidated = false;
    private taskSnapshotInFlight = false;
    private destroyed = false;

    readonly chat: Y.Doc;
    readonly session: Y.Doc;

    constructor(private readonly options: SessionDocsOptions = {}, importedModel?: DocModel) {
        this.model = importedModel ?? new DocModel(
            () => this.options.now?.() ?? new Date().toISOString(),
            this.options.payloadBudgets ? new ToolPayloads(
                this.options.payloadBudgets.inlineBytes,
                this.options.payloadBudgets.externalBytes,
                this.options.payloadBudgets.maxPayloadBytes,
            ) : undefined,
        );
        this.turns = new TurnMachine(this.model);
        this.chatProjection = new ChatProjection(this.model, this.turns);
        this.historyMerge = new HistoryMerge(this.model);
        this.sessionProjection = new SessionProjection(this.model, this.turns);
        this.taskProjection = new TaskProjection(this.model);
        this.inputQueue = new InputQueueProjection(this.model);
        this.interaction = new InteractionProjection(this.model);
        this.chat = this.model.chat;
        this.session = this.model.session;
    }

    /** Exact immutable JSON for a current ToolPayloadRef; stale or unknown versions are absent. */
    readPayload(id: string): string | undefined { return this.model.payloads.read(id); }

    /**
     * 从展示 checkpoint 构造全新文档（import 不作用于任何活跃 doc）。
     * 活动 turn/interactions 收敛为中断/失效；缺失引用或未知版本显式拒绝。
     */
    static fromCheckpoint(checkpoint: unknown, options: SessionDocsOptions = {}): SessionDocs {
        const docs = new SessionDocs(options);
        applyCheckpoint(checkpoint, docs.model);
        return docs;
    }

    /** 当前文档的公开展示材料；payload 全文随 checkpoint 一起导出，引用必须可解析。 */
    exportCheckpoint(): DisplayCheckpoint { return exportCheckpoint(this.model); }

    /** Ingest a received batch without publishing a Yjs transaction per ACP notification. */
    acceptBatch(notifications: Iterable<JsonRpcNotification>): void {
        if (this.destroyed) return;
        this.model.transactBoth(() => { for (const notification of notifications) this.accept(notification); });
    }

    /**
     * 一次安装本次初始 load 的回放批次（REQ-H04）：整批只发一次 Yjs 事务，
     * 观察者看到的是「装完」的快照，不存在半份首屏。
     */
    acceptHistoryBatch(notifications: Iterable<JsonRpcNotification>): void {
        if (this.destroyed) return;
        this.acceptBatch(notifications);
    }

    /** 记录本次初始 load 的浏览身份与首游标；响应未协商分页时窗口为空。 */
    seedHistoryWindow(response: unknown): void {
        if (this.destroyed) return;
        const value = object(response);
        const snapshotToken = string(value?.snapshotToken);
        this.historyWindow = snapshotToken
            ? { snapshotToken, nextCursor: string(value?.nextCursor) }
            : null;
        this.historyInvalidated = false;
    }

    /** 翻页进度：更早方向的下一游标；`null` = 没有可用游标（未协商／已作废／没有更早内容）。 */
    historyCursor(): string | null { return this.historyWindow?.nextCursor ?? null; }
    /** 窗口是否存在：有窗口但游标为 `null` 表示**更早方向确实没有内容**（验收 A15）。 */
    historyWindowOpen(): boolean { return this.historyWindow !== null; }
    /** 窗口是否因 rewind 作废（不是「本连接从未协商过分页」）。 */
    historyWindowInvalidated(): boolean { return this.historyInvalidated; }

    /** rewind：范围真的变了，游标与浏览身份一起作废，翻页要等下一次初始 load 才恢复。 */
    invalidateHistoryWindow(): void {
        this.historyWindow = null;
        this.historyInvalidated = true;
    }

    /**
     * 合并一页历史（REQ-C01、REQ-H01-3）：按 `entryId` + 子项位置幂等并进正确区间，
     * 只改历史窗口，不碰实时执行状态、plan／任务／输入队列与未决交互。
     */
    mergeHistoryPage(sessionId: string, response: unknown): HistoryPageMerge {
        if (this.destroyed) return { applied: 0, nextCursor: null, status: "stale" };
        const value = object(response);
        const items = decodeHistoryPageItems(value?.items);
        const snapshotToken = string(value?.snapshotToken);
        const responseSession = string(value?.sessionId);
        if (!value || items === null) return { applied: 0, nextCursor: null, status: "malformed" };
        const window = this.historyWindow;
        if (!window || !snapshotToken || snapshotToken !== window.snapshotToken
            || (responseSession !== null && responseSession !== sessionId))
            return { applied: 0, nextCursor: null, status: "stale" };
        let applied = 0;
        this.model.transactBoth(() => {
            for (const item of items) if (this.historyMerge.applyEntry(item, item.subItems)) applied++;
        });
        const nextCursor = string(value.nextCursor);
        this.historyWindow = { snapshotToken, nextCursor };
        return { applied, nextCursor, status: "merged" };
    }

    destroy(): void {
        if (this.destroyed) return;
        this.destroyed = true;
        this.interaction.abortAll();
        this.taskProjection.setSnapshotRequester(null);
        this.seenEvents.clear();
        this.historyMerge.reset();
        this.invalidateHistoryWindow();
        this.model.destroy();
    }
    completeTurn(status: TurnExitStatus = "completed", stopReason?: string): boolean {
        if (this.destroyed) return false;
        const exited = this.turns.exit(status, stopReason);
        this.interaction.abortAll();
        return exited;
    }
    requestCancel(): { turnId: string; previous: "accepting" | "running" } | null {
        if (this.destroyed) return null;
        const token = this.turns.requestCancel();
        if (token) this.interaction.abortAll();
        return token;
    }
    restoreCancel(token: { turnId: string; previous: "accepting" | "running" }): void {
        if (!this.destroyed) this.turns.restoreCancel(token);
    }
    acceptDeliveredUserInput(inputId: string, text: string): boolean {
        if (this.destroyed) return false;
        this.interaction.abortAll();
        return this.chatProjection.acceptDeliveredUserInput(inputId, text);
    }
    handleRequest(method: string, params: unknown, expectedSessionId: string | undefined, options: InteractionCallbacks): Promise<unknown> {
        if (this.destroyed) return Promise.reject(new Error("SessionDocs is destroyed"));
        return this.interaction.handle(method, params, expectedSessionId, options);
    }

    /** Repair missing background-task revisions through Peri's session/bg-tasks snapshot. */
    setTaskSnapshotRequester(requester: (() => Promise<unknown>) | null): void {
        if (this.destroyed) return;
        this.taskProjection.setSnapshotRequester(requester ? () => {
            if (this.taskSnapshotInFlight || this.destroyed) return;
            this.taskSnapshotInFlight = true;
            let pending: Promise<unknown>;
            try { pending = requester(); }
            catch { this.taskSnapshotInFlight = false; return; }
            void pending.then((snapshot) => {
                if (!this.destroyed) this.taskProjection.acceptBackground("bg-task-snapshot", object(snapshot));
            }).catch(() => {
                // A later task delta retries; the previous revision remains untrusted.
            }).finally(() => { this.taskSnapshotInFlight = false; });
        } : null);
    }

    /** Seed queue state from session/input/snapshot, even when no queue notification has arrived. */
    seedInputQueue(snapshot: unknown): void { if (!this.destroyed) this.inputQueue.seed(snapshot); }
    seedConfig(response: unknown): void { if (!this.destroyed) this.sessionProjection.seedConfig(response); }

    accept(notification: JsonRpcNotification): void {
        if (this.destroyed) return;
        if (!notification || typeof notification !== "object") return;
        const params = object(notification.params);
        if (!params) return;
        const eventId = string(object(object(params._meta)?.peri)?.eventId);
        if (eventId && this.seenEvents.has(eventId)) return;

        let accepted = false;
        if (notification.method === "session/update") {
            const update = object(params.update);
            if (!update) return;
            // 带条目身份的回放／历史页子项走历史投影：不建 active turn、不伪造用户输入。
            const entryRef = historyEntryRefOf(update);
            if (entryRef) {
                accepted = this.historyMerge.applyEntry(entryRef, [{ position: null, update }]);
            } else {
                const previousTurn = this.turns.activeTurn();
                accepted = this.chatProjection.accept(update, params) || this.sessionProjection.acceptUpdate(update, params);
                if (string(update.sessionUpdate) === "user_message_chunk" && previousTurn !== this.turns.activeTurn()) {
                    this.interaction.abortAll();
                }
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
            // REQ-C02 / A12：Compact 不整表替换。模型上下文收缩不等于界面删除历史，
            // 已加载窗口、翻页 cursor 进度、当前 turn／流式输出与未决交互全部保留；
            // 这里只更新既有 Compact 展示语义（compaction / compactionStatus）。
            return this.sessionProjection.acceptAgentEvent(type, value);
        }
        if (type === "rewind_completed" && value) {
            // 与 Compact 分开处理：rewind 是范围真的变了（服务端推进结构版本、删除尾部行），
            // 窗口失效并刷新（`replaceHistory`），旧游标一并作废；但执行状态与未决交互保留。
            const refreshed = this.chatProjection.replaceHistory(value.messages_json, "preserve-execution");
            this.historyMerge.reset();
            this.invalidateHistoryWindow();
            const observed = this.sessionProjection.acceptAgentEvent(type, value);
            return refreshed || observed;
        }
        return this.inputQueue.acceptAgentEvent(type, value)
            || this.sessionProjection.acceptAgentEvent(type, value)
            || this.taskProjection.acceptSubagent(type, value);
    }
}
