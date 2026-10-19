import type { Agent } from "./agent";
import { SendReceipt } from "./send-receipt";
import type { JsonRpcNotification, Transport } from "../transport/types";
import type { SessionDocs } from "../state/session-docs";
import { historyGenerationOf } from "../state/history-merge";
import { EventQueue } from "../transport/event-queue";
import { initializeAcpClient } from "./acp-handshake";
import { closeDrainBudget, closeUntilSettled, type CloseOptions } from "./session-close";

/** 一页历史的合并结果（`applied` = 本次真正并进视图的条目数）。 */
export type HistoryPageOutcome = { applied: number; nextCursor: string | null };

/** 翻页不可用的原因：能力未协商、没有更早内容，或浏览窗口已被 rewind／新 load 作废。 */
export type HistoryPageUnavailableReason = "not_negotiated" | "exhausted" | "invalidated";

/** `session/load` + cursor 无法发起（没有可用游标）。 */
export class HistoryPageUnavailableError extends Error {
    constructor(readonly reason: HistoryPageUnavailableReason, message: string) {
        super(message);
        this.name = "HistoryPageUnavailableError";
    }
}

/** 迟到的历史页：会话已切换／已关闭，或浏览快照已变——整页丢弃，不得污染当前视图。 */
export class StaleHistoryPageError extends Error {
    constructor(readonly reason: "session" | "window", message: string) {
        super(message);
        this.name = "StaleHistoryPageError";
    }
}

/** `session/input/*` 的队列身份、条目状态与回执；执行调度归 Rust Agent mailbox。 */
type QueueSnapshot = {
    generation: string;
    revision?: number;
    activeRequestId?: string;
    items?: Array<{ inputId: string; state: string }>;
};
type QueueReceipt = {
    results: Array<{ inputId: string; state: string }>;
    snapshot?: QueueSnapshot;
};
type SessionState = "declared" | "starting" | "active" | "cleanup-pending" | "closed";
type PendingDelivery = {
    resolve: () => void;
    reject: (error: Error) => void;
};

/** 一次 `session/new` 或 `session/load` 得到的会话身份。 */
type SessionSetup = { modes?: unknown; configOptions?: unknown[]; sessionId?: string };

/**
 * 单个 Session 的 ACP 客户端。传输由嵌入方注入（WASM Host 或自定义 transport），
 * SDK 只按 ACP 方法语义驱动：输入队列、取消通知、显式关闭与会话文档投影。
 */
export class Session {
    private state: SessionState = "declared";
    private transport?: Transport;
    private sessionId?: string;
    private currentPath?: string;
    private generation?: string;
    private startPromise?: Promise<Session>;
    private closing?: Promise<void>;
    private startupCleanupPromise?: Promise<void>;
    private notifications = new EventQueue(() => {});
    private unsubscribe?: () => void;
    private streamOpen = false;
    private streamEnded = false;
    private readonly pendingDeliveries = new Map<string, PendingDelivery>();
    private readonly pendingInputText = new Map<string, string>();
    /** 相同 cursor + 相同 limit 的并发翻页合并为一次请求（REQ-C01-2、验收 A08）。 */
    private readonly pendingPages = new Map<string, Promise<HistoryPageOutcome>>();

    constructor(private readonly agent: Agent) {}

    get id(): string {
        if (!this.sessionId) throw new Error("Session has not started");
        return this.sessionId;
    }

    get docs(): SessionDocs {
        return this.agent.docs;
    }

    get path(): string | undefined { return this.currentPath; }
    get isClosed(): boolean { return this.state === "closed"; }

    start(requestedSessionId: string | null): Promise<Session> {
        if (this.closing || this.startupCleanupPromise)
            throw new Error("Session cannot start while closing");
        if (this.state !== "declared")
            throw new Error(`Session cannot start from ${this.state}`);
        if (requestedSessionId === "")
            return Promise.reject(new TypeError("Session id must be nonempty or null"));
        this.state = "starting";
        this.startPromise = this.startOnce(requestedSessionId);
        return this.startPromise;
    }

    private async startOnce(requestedSessionId: string | null): Promise<Session> {
        const claims = this.agent.claims;
        let transport: Transport | undefined;
        try {
            await claims.claimAgent();
            if (requestedSessionId !== null) await claims.claimSession(requestedSessionId);
            const path = requestedSessionId === null
                ? this.agent.path
                : (await this.agent.options.sandbox.getSession(requestedSessionId))?.cwd;
            if (!path) throw new Error(requestedSessionId === null
                ? "Agent path is required for a new Session"
                : `Session not found: ${requestedSessionId}`);
            transport = await this.agent.options.sandbox.createTransport(path);
            this.transport = transport;
            transport.setRequestHandler((method, params) => this.docs.handleRequest(method, params, this.sessionId, this.agent.options));
            let replayTurnPending = false;
            // 初始 load 的代次（REQ-H04、验收 A22）：订阅在本请求**之前**建立，所以
            // 回放通知一条都不会漏；本代次的通知先暂存，响应成功后才一次安装。
            const loadGeneration = requestedSessionId === null
                ? undefined
                : this.agent.beginLoadGeneration(crypto.randomUUID());
            const stagedReplay: JsonRpcNotification[] = [];
            let replayInstalled = false;
            this.unsubscribe = transport.subscribe((event) => {
                const rawEvent = event.method === "session/update" || event.method === "peri/agent_event";
                const stateEvent = rawEvent ||
                    event.method === "peri/agent_event_done" ||
                    event.method === "peri/unstable_event";
                if (!stateEvent) return;
                if (this.sessionId && (event.params as { sessionId?: string } | undefined)?.sessionId !== this.sessionId) return;
                const generation = historyGenerationOf(event);
                if (generation !== null) {
                    // 历史回放：非本代次（迟到 / 被取代）直接丢弃；本代次在响应前只暂存。
                    if (generation !== loadGeneration || !this.agent.isCurrentLoadGeneration(generation)) return;
                    // 暂存只约束**文档安装**：原始诊断流不是投影，连接收到什么就流什么，
                    // 首屏回放同样不得缺席（否则 README/examples 的原始事件流会缺首屏）。
                    if (rawEvent) {
                        this.acceptDeliveryEvent(event);
                        this.notifications.push(event);
                    }
                    // 响应已把整批回放一次安装：之后同代次再来的回放通知是重复投递。
                    // 回放路径没有子项下标，重复投影只会把同一段内容复制成两个块，这里整条丢弃。
                    if (replayInstalled) return;
                    stagedReplay.push(event);
                    return;
                }
                if (event.method === "session/update") {
                    const update = (event.params as { update?: { sessionUpdate?: string; _meta?: { periReplay?: boolean } } } | undefined)?.update;
                    if (update?.sessionUpdate === "user_message_chunk" || update?.sessionUpdate === "agent_message_chunk" ||
                        update?.sessionUpdate === "agent_thought_chunk" || update?.sessionUpdate === "tool_call" ||
                        update?.sessionUpdate === "tool_call_update")
                        replayTurnPending = update._meta?.periReplay === true;
                } else if (event.method === "peri/agent_event_done") {
                    replayTurnPending = false;
                }
                this.project(event);
                if (!rawEvent) return;
                this.acceptDeliveryEvent(event);
                this.notifications.push(event);
            });
            await initializeAcpClient(transport, {
                capabilities: {
                    "peri.userInputQueue": true,
                    "peri.agentEvent": true,
                    "peri.sessionWorkspaceV1": true,
                    "peri.agentEventDone": true,
                    "peri.replay": true,
                    "peri.tokenStats": true,
                    "peri.planEntryActiveForm": true,
                    "peri.unstableEvent": true,
                    // REQ-C01 / REQ §5.3：分页走协商通道。少声明一项就等于放弃该通道——
                    // 服务端对未协商的连接拒绝带 cursor 的 session/load，且初始回放不带
                    // 条目位置戳（periEntryId / periPosition），客户端无法做区间合并。
                    "peri.historyPagination": true,
                },
            });
            const params = { cwd: path, mcpServers: this.agent.mcpServers() };
            let id: string;
            if (requestedSessionId === null) {
                const created = await transport.request<SessionSetup>("session/new", {
                    ...params,
                    ...(this.agent.options.instructions
                        ? { _meta: { "peri.instructions": this.agent.options.instructions } }
                        : {}),
                });
                id = created.sessionId ?? "";
                if (!id) throw new Error("Peri returned no sessionId");
                this.docs.seedConfig(created);
                await claims.claimSession(id);
            } else {
                const loaded = await transport.request<SessionSetup & { _meta?: { periLoadGeneration?: string } }>(
                    "session/load",
                    { ...params, sessionId: requestedSessionId, loadGeneration },
                );
                // 响应必须属于本次代次：被新 load 取代后，这一份既不能安装也不能宣称就绪。
                const echoed = loaded._meta?.periLoadGeneration;
                if (loadGeneration && echoed !== undefined && echoed !== loadGeneration)
                    throw new Error("session/load response belongs to a superseded load generation");
                if (loadGeneration && !this.agent.isCurrentLoadGeneration(loadGeneration))
                    throw new Error("session/load was superseded by a newer load generation");
                // 回放完整后**一次安装**：整批一个 Yjs 事务，之后才发布窗口身份与游标。
                if (stagedReplay.length) this.docs.acceptHistoryBatch(stagedReplay);
                replayInstalled = true;
                stagedReplay.length = 0;
                this.docs.seedConfig(loaded);
                this.docs.seedHistoryWindow(loaded);
                // ACP 在 load 响应前完成历史回放，且没有独立的回放 turn-done 通知。
                // 只有未带条目身份的回放（未协商分页的连接）才需要收口实时 turn。
                if (replayTurnPending) this.docs.completeTurn();
                id = requestedSessionId;
            }
            const snapshot = await transport.request<QueueSnapshot>("session/input/snapshot", { sessionId: id });
            if (!snapshot.generation) throw new Error("Peri returned no input queue generation");
            this.docs.seedInputQueue(snapshot);
            this.sessionId = id;
            this.currentPath = path;
            this.generation = snapshot.generation;
            this.docs.setTaskSnapshotRequester(() => this.activeTransport().request("session/bg-tasks", { sessionId: id }));
            this.state = "active";
            return this;
        } catch (error) {
            this.state = "cleanup-pending";
            this.unsubscribe?.();
            this.unsubscribe = undefined;
            await this.notifications.return();
            this.notifications = new EventQueue(() => {});
            this.agent.discardFailedSessionDocs();
            try {
                await this.releaseResources();
            } catch (cleanupError) {
                throw new AggregateError(
                    [error, cleanupError],
                    `Session startup failed: ${String(error)}; cleanup failed: ${String(cleanupError)}. Retry cleanupStartup before starting another Session.`,
                    { cause: error },
                );
            }
            this.state = "declared";
            throw error;
        }
    }

    /** 通知进投影：失败只记录，不吞掉交付处理与原始 ACP 事件流。 */
    private project(event: JsonRpcNotification): void {
        try {
            this.docs.accept(event);
        } catch (error) {
            console.error("Failed to project ACP notification into SessionDocs", error);
        }
    }

    /** 更早方向是否还有可用游标（翻页入口的前置判断，不发起请求）。 */
    get canLoadEarlierHistory(): boolean { return this.docs.historyCursor() !== null; }

    /**
     * 向上读取更早一页历史（REQ-C01、验收 A11/A20/A23/A24）。
     *
     * 入口仍是 `session/load`，只带 `{sessionId, cursor, limit}`——cursor 由 SDK 持有并推进，
     * 调用方只表达「还要更早的一页」。相同 cursor + 相同 limit 的并发调用合并为一次请求；
     * 失败保留当前画面与运行状态（游标不动），可直接重试。切会话／rewind 之后到达的旧页
     * 抛 [`StaleHistoryPageError`]，不并入视图。
     */
    async loadEarlierHistory(limit?: number): Promise<HistoryPageOutcome> {
        const transport = this.activeTransport();
        const sessionId = this.id;
        const cursor = this.docs.historyCursor();
        if (!cursor) {
            // 三种「没有游标」不是一回事：窗口被作废、确实没有更早内容（A15）、本连接未协商。
            const reason: HistoryPageUnavailableReason = this.docs.historyWindowInvalidated()
                ? "invalidated"
                : this.docs.historyWindowOpen() ? "exhausted" : "not_negotiated";
            throw new HistoryPageUnavailableError(reason, {
                invalidated: "History window is invalidated; reload the session before paging further",
                exhausted: "This browsing window has no earlier history left",
                not_negotiated: "This connection has no history pagination window",
            }[reason]);
        }
        const key = `${cursor}|${limit ?? "default"}`;
        const inFlight = this.pendingPages.get(key);
        if (inFlight) return inFlight;
        const pending = this.fetchHistoryPage(transport, sessionId, cursor, limit);
        this.pendingPages.set(key, pending);
        try {
            return await pending;
        } finally {
            if (this.pendingPages.get(key) === pending) this.pendingPages.delete(key);
        }
    }

    private async fetchHistoryPage(
        transport: Transport,
        sessionId: string,
        cursor: string,
        limit: number | undefined,
    ): Promise<HistoryPageOutcome> {
        // 历史页请求不得携带 setup 参数：分页不重新装配、不改绑定（REQ §5.2）。
        const response = await transport.request<unknown>("session/load", {
            sessionId,
            cursor,
            ...(limit === undefined ? {} : { limit }),
        });
        // 切会话／关闭之后到达的页整页丢弃（REQ-C01-5、验收 A11）。
        if (this.state !== "active" || this.sessionId !== sessionId)
            throw new StaleHistoryPageError("session", "History page arrived after the session changed");
        const merged = this.docs.mergeHistoryPage(sessionId, response);
        if (merged.status === "stale")
            throw new StaleHistoryPageError("window", "History page belongs to an expired browsing snapshot");
        if (merged.status === "malformed")
            throw new Error("session/load returned a malformed history page");
        return { applied: merged.applied, nextCursor: merged.nextCursor };
    }

    private async releaseResources(): Promise<void> {
        const transport = this.transport;
        if (transport) {
            // 关闭未确认时保留引用，重试必须能再次尝试同一个 transport。
            await transport.close();
            if (this.transport === transport) this.transport = undefined;
        }
        await this.agent.claims.release();
    }

    private activeTransport(): Transport {
        if (this.closing || this.state !== "active" || !this.transport)
            throw new Error("Session is not active");
        return this.transport;
    }

    private domainTransport(): Transport {
        if (!this.transport) throw new Error("Session has no transport");
        return this.transport;
    }

    send(text: string): SendReceipt {
        this.activeTransport();
        if (!text) throw new TypeError("Cannot send empty user input");
        return new SendReceipt(this, text);
    }

    trackInput(inputId: string, text: string): void {
        this.pendingInputText.set(inputId, text);
    }

    waitForDelivery(inputId: string): Promise<void> {
        return new Promise((resolve, reject) => this.pendingDeliveries.set(inputId, { resolve, reject }));
    }

    confirmDelivery(inputId: string): void {
        const pending = this.pendingDeliveries.get(inputId);
        if (!pending) return;
        const text = this.pendingInputText.get(inputId);
        if (text) this.docs.acceptDeliveredUserInput(inputId, text);
        this.pendingInputText.delete(inputId);
        this.pendingDeliveries.delete(inputId);
        pending.resolve();
    }

    rejectDelivery(inputId: string, error: unknown): void {
        this.pendingInputText.delete(inputId);
        const pending = this.pendingDeliveries.get(inputId);
        if (!pending) return;
        this.pendingDeliveries.delete(inputId);
        pending.reject(error instanceof Error ? error : new Error(String(error)));
    }

    private acceptDeliveryEvent(event: JsonRpcNotification): void {
        if (event.method !== "peri/agent_event") return;
        const eventJson = (event.params as { event_json?: unknown } | undefined)?.event_json;
        if (typeof eventJson !== "string") return;
        let parsed: unknown;
        try { parsed = JSON.parse(eventJson); }
        catch { return; }
        if (!parsed || typeof parsed !== "object") return;
        const payload = parsed as { type?: string; value?: { input_id?: string; generation?: string } };
        const value = payload.value;
        if (!value) return;
        if (payload.type === "user_input_delivered" && value.generation === this.generation &&
            typeof value.input_id === "string")
            this.confirmDelivery(value.input_id);
    }

    private queueReceiptState(receipt: QueueReceipt, inputId: string): string {
        const state = receipt.results.find((item) => item.inputId === inputId)?.state;
        if (!state || state === "unknown" || state === "withdrawn")
            throw new Error(`Input not accepted: ${state ?? "missing"}`);
        if (receipt.snapshot) this.docs.seedInputQueue(receipt.snapshot);
        return state;
    }

    /** 入队即由 Rust mailbox 决定调度；SDK 不另行申请执行准入。 */
    async enqueue(inputId: string, text: string): Promise<string> {
        const receipt = await this.activeTransport().request<QueueReceipt>("session/input/enqueue", {
            sessionId: this.id,
            generation: this.generation,
            commandId: `${inputId}:enqueue`,
            inputId,
            content: text,
            originalDraft: text,
        });
        return this.queueReceiptState(receipt, inputId);
    }

    async dispatch(inputId: string): Promise<void> {
        const receipt = await this.activeTransport().request<QueueReceipt>("session/input/dispatch", {
            sessionId: this.id,
            generation: this.generation,
            commandId: `${inputId}:dispatch`,
            inputIds: [inputId],
        });
        this.queueReceiptState(receipt, inputId);
    }

    /** 撤回队列中尚未派发的输入；`withdrawn` 是本次撤回成功的结果状态。 */
    async takeBack(inputId: string): Promise<void> {
        const receipt = await this.activeTransport().request<QueueReceipt>("session/input/takeback", {
            sessionId: this.id,
            generation: this.generation,
            commandId: `${inputId}:takeback`,
            inputId,
        });
        const state = receipt.results.find((item) => item.inputId === inputId)?.state;
        if (receipt.snapshot) this.docs.seedInputQueue(receipt.snapshot);
        if (!state || state === "unknown")
            throw new Error(`Input not taken back: ${state ?? "missing"}`);
    }

    /** 当前 ACP 的取消是一次会话级通知；未完成 turn 的收敛由 Peri 决定。 */
    async cancel(): Promise<void> {
        await this.activeTransport().notify("session/cancel", { sessionId: this.id });
    }

    /** 原始 ACP 通知：session/update 与 peri/agent_event。 */
    async *stream(): AsyncIterable<JsonRpcNotification> {
        this.activeTransport();
        if (this.streamOpen)
            throw new Error("This Session already has an event consumer");
        this.streamOpen = true;
        try {
            while (!this.streamEnded) {
                const next = await this.notifications.next();
                if (next.done) break;
                const event = next.value;
                if (event && (event.params as { sessionId?: string } | undefined)?.sessionId === this.id)
                    yield event;
            }
        } finally {
            this.streamOpen = false;
            await this.notifications.return();
            if (!this.streamEnded) this.notifications = new EventQueue(() => {});
        }
    }

    close(options: CloseOptions = {}): Promise<void> {
        if (this.state === "closed") return Promise.resolve();
        closeDrainBudget(options);
        if (!this.closing) {
            this.closing = Promise.resolve()
                .then(() => this.closeOnce(options))
                .finally(() => { this.closing = undefined; });
            void this.closing.catch(() => {});
        }
        return this.closing;
    }

    private async closeOnce(options: CloseOptions): Promise<void> {
        const transport = this.domainTransport();
        const sessionId = this.id;
        await closeUntilSettled(() => transport.request("session/close", { sessionId }), options);
        this.state = "cleanup-pending";
        this.docs.setTaskSnapshotRequester(null);
        await this.releaseResources();
        this.docs.completeTurn("cancelled");
        this.unsubscribe?.();
        this.unsubscribe = undefined;
        this.streamEnded = true;
        for (const inputId of this.pendingDeliveries.keys())
            this.rejectDelivery(inputId, new Error("Session closed before user input was delivered"));
        await this.notifications.return();
        this.state = "closed";
    }

    cleanupStartup(): Promise<void> {
        if (this.sessionId || this.state === "active" || this.state === "closed")
            throw new Error("Startup cleanup cannot shut down an established Session");
        if (this.state === "starting")
            throw new Error("Wait for Session startup to finish before cleanupStartup");
        if (!this.startupCleanupPromise) {
            this.startupCleanupPromise = this.releaseResources().then(() => {
                this.state = "declared";
            }).finally(() => { this.startupCleanupPromise = undefined; });
        }
        return this.startupCleanupPromise;
    }
}
