import type { JsonRpcNotification, Transport } from "../transport/types";
import { EventQueue } from "../transport/event-queue";
import { raceAbort, throwIfAborted } from "../util/abort";
import { initializeAcpClient } from "./acp-handshake";
import { closeDrainBudget, closeUntilSettled, type CloseOptions } from "./session-close";

/**
 * 单个 Session 的纯 ACP 客户端。
 *
 * 传输由调用方注入（WASM 宿主租约或自定义 transport）；本类只做 `initialize`、
 * `session/new|load`、输入队列命令、取消、翻页请求、`session/close` 与原始诊断流。
 * 它不 claim、不关闭宿主、不做 Yjs 投影，也不依赖 Agent/展示门面。
 */

/** 启动阶段；供上层映射为 SessionCreationError 的 `stage`。 */
type SessionStartStage =
    | "initialize" | "mcp-connect" | "session/new" | "claim"
    | "session/load" | "input-snapshot";

/** `session/new|load` 后的会话身份、初始化材料与队列快照。 */
export type SessionStartResult = {
    sessionId: string;
    path: string;
    generation: string;
    /** 原始 `session/new|load` 响应；供上层输出 seed config。 */
    seed: unknown;
    /** 本次 load 代次的回放通知，响应成功后一次性交付。 */
    historyBatch: JsonRpcNotification[];
    history: { snapshotToken: string | null; nextCursor: string | null };
    /** `session/input/snapshot` 响应。 */
    queueSnapshot: unknown;
    /** 未协商分页的旧式回放需要在安装后收口 turn。 */
    replayTurnPending: boolean;
};

export class SessionStartError extends Error {
    constructor(
        readonly stage: SessionStartStage,
        readonly createdSessionId: string | null,
        message: string,
        options?: { cause?: unknown },
    ) {
        super(message, options);
        this.name = "SessionStartError";
    }
}

type SessionStartRequest = {
    path: string;
    /** `null`/省略 = `session/new`；字符串 = `session/load`。 */
    sessionId?: string | null;
    instructions?: string;
    mcpServers?: unknown;
};

/** 启动控制：取消只中止可安全脱离的等待，`session/new` 保留创建身份。 */
type SessionStartOptions = {
    signal?: AbortSignal;
    /** `session/new` 返回后、后续阶段前回调；用于先登记索引与 claim。 */
    onSessionCreated?: (sessionId: string) => void | Promise<void>;
};

interface SessionClientOptions {
    /** 反向 ACP 请求（`mcp/message`、权限、elicitation）；由宿主装配路由。 */
    requestHandler: (method: string, params: unknown) => unknown | Promise<unknown>;
    /** 过滤/暂存后的状态通知；初始化事实发布后由 `beginLive()` 放行。 */
    onNotification?: (notification: JsonRpcNotification) => void;
    /** `session/input/*` 回执携带的队列快照。 */
    onQueueSnapshot?: (snapshot: unknown) => void;
    /** 用户输入已投递（文本为本地登记的内容；外部输入可能为空）。 */
    onDelivered?: (inputId: string, text: string) => void;
    /** initialize 之后、`session/new|load` 之前（MCP 反向通道就绪）。 */
    onBeforeSession?: () => Promise<void>;
}

type SessionState = "declared" | "starting" | "active" | "cleanup-pending" | "closed";
type PendingDelivery = { promise: Promise<void>; resolve: () => void; reject: (error: Error) => void };
type QueueSnapshot = { generation?: unknown };
type QueueReceipt = { results?: Array<{ inputId?: string; state?: string }>; snapshot?: unknown };

const STATE_EVENTS = new Set([
    "session/update", "peri/agent_event", "peri/agent_event_done", "peri/unstable_event",
]);
const RAW_EVENTS = new Set(["session/update", "peri/agent_event"]);
const REPLAY_KINDS = new Set([
    "user_message_chunk", "agent_message_chunk", "agent_thought_chunk", "tool_call", "tool_call_update",
]);
/** 启动期间暂存 live 通知的上限；超出显式记录，不无限增长。 */
const MAX_DEFERRED_LIVE = 4096;

function record(value: unknown): Record<string, unknown> | null {
    return value !== null && typeof value === "object" && !Array.isArray(value)
        ? value as Record<string, unknown>
        : null;
}

function nonEmpty(value: unknown): string | null {
    return typeof value === "string" && value.length > 0 ? value : null;
}

/** 回放通知的 load 代次（`_meta.periLoadGeneration`）。 */
function loadGenerationOf(notification: { params?: unknown }): string | null {
    const params = record(notification.params);
    if (!params) return null;
    const updateMeta = record(record(params.update)?._meta);
    const paramsMeta = record(params._meta);
    return nonEmpty(updateMeta?.periLoadGeneration) ?? nonEmpty(paramsMeta?.periLoadGeneration);
}

function decodeAgentEvent(raw: unknown): { type: string; value: Record<string, unknown> } | null {
    if (typeof raw !== "string") return null;
    let parsed: unknown;
    try { parsed = JSON.parse(raw); } catch { return null; }
    const event = record(parsed);
    const type = nonEmpty(event?.type);
    const value = record(event?.value);
    return type && value ? { type, value } : null;
}

export class Session {
    private state: SessionState = "declared";
    private readonly transport: Transport;
    private sessionId?: string;
    private createdSessionId?: string;
    private requestedSessionId?: string;
    private currentPath?: string;
    private queueGeneration?: string;
    private closing?: Promise<void>;
    private startupCleanupPromise?: Promise<void>;
    private loadGeneration?: string;
    private replayInstalled = false;
    private replayTurnPending = false;
    private readonly stagedReplay: JsonRpcNotification[] = [];
    private readonly deferredLive: JsonRpcNotification[] = [];
    private live = false;
    private notifications = new EventQueue(() => {});
    private unsubscribe?: () => void;
    private streamOpen = false;
    private streamEnded = false;
    private readonly pendingDeliveries = new Map<string, PendingDelivery>();
    private readonly pendingInputText = new Map<string, string>();
    private readonly deliveredInputs = new Set<string>();

    constructor(transport: Transport, private readonly options: SessionClientOptions) {
        this.transport = transport;
        transport.setRequestHandler((method, params) => options.requestHandler(method, params));
    }

    get id(): string {
        if (!this.sessionId) throw new Error("Session has not started");
        return this.sessionId;
    }

    /** 已建立的会话身份（启动失败但 `session/new` 已返回时也保留，供清理）。 */
    get establishedSessionId(): string | null { return this.sessionId ?? null; }
    get path(): string | undefined { return this.currentPath; }
    get generation(): string | undefined { return this.queueGeneration; }
    get isActive(): boolean { return this.state === "active" && !this.closing; }
    get isClosed(): boolean { return this.state === "closed"; }

    start(request: SessionStartRequest, options: SessionStartOptions = {}): Promise<SessionStartResult> {
        if (this.state !== "declared")
            throw new Error(`Session cannot start from ${this.state}`);
        if (!request.path) throw new TypeError("Session path is required");
        if (request.sessionId === "")
            throw new TypeError("Session id must be nonempty or null");
        this.state = "starting";
        this.requestedSessionId = request.sessionId ?? undefined;
        this.currentPath = request.path;
        return this.startOnce(request, options);
    }

    private async startOnce(request: SessionStartRequest, control: SessionStartOptions): Promise<SessionStartResult> {
        let stage: SessionStartStage = "initialize";
        const signal = control.signal;
        this.unsubscribe = this.transport.subscribe((event) => this.receive(event));
        try {
            throwIfAborted(signal, "ACP initialize");
            await raceAbort(initializeAcpClient(this.transport, {
                capabilities: {
                    "peri.userInputQueue": true,
                    "peri.agentEvent": true,
                    "peri.sessionWorkspaceV1": true,
                    "peri.agentEventDone": true,
                    "peri.replay": true,
                    "peri.tokenStats": true,
                    "peri.planEntryActiveForm": true,
                    "peri.unstableEvent": true,
                    "peri.historyPagination": true,
                },
            }), signal, "ACP initialize");
            stage = "mcp-connect";
            await raceAbort(
                Promise.resolve().then(() => this.options.onBeforeSession?.()),
                signal,
                "MCP bridge connect",
            );
            const params = { cwd: request.path, mcpServers: request.mcpServers ?? [] };
            let seed: unknown;
            let historyBatch: JsonRpcNotification[] = [];
            let history: SessionStartResult["history"] = { snapshotToken: null, nextCursor: null };
            let replayTurnPending = false;
            if (request.sessionId === null || request.sessionId === undefined) {
                stage = "session/new";
                // `session/new` 可能已经创建会话：不 race，保留 createdSessionId 交给正常清理。
                const created = await this.transport.request<{ sessionId?: unknown }>("session/new", {
                    ...params,
                    ...(request.instructions
                        ? { _meta: { "peri.instructions": request.instructions } }
                        : {}),
                });
                const id = nonEmpty(created.sessionId);
                if (!id) throw new Error("Peri returned no sessionId");
                this.createdSessionId = id;
                this.sessionId = id;
                seed = created;
                stage = "claim";
                throwIfAborted(signal, "Session claim");
                await control.onSessionCreated?.(id);
            } else {
                stage = "session/load";
                const generation = crypto.randomUUID();
                this.loadGeneration = generation;
                const loaded = await raceAbort(this.transport.request<{
                    _meta?: { periLoadGeneration?: unknown };
                }>("session/load", {
                    ...params,
                    sessionId: request.sessionId,
                    loadGeneration: generation,
                }), signal, "session/load");
                const echoed = nonEmpty(record(loaded._meta)?.periLoadGeneration);
                if (echoed !== null && echoed !== generation)
                    throw new Error("session/load response belongs to a superseded load generation");
                this.sessionId = request.sessionId;
                seed = loaded;
                historyBatch = [...this.stagedReplay];
                this.stagedReplay.length = 0;
                this.replayInstalled = true;
                const loadedRecord = record(loaded);
                history = {
                    snapshotToken: nonEmpty(loadedRecord?.snapshotToken),
                    nextCursor: nonEmpty(loadedRecord?.nextCursor),
                };
                replayTurnPending = this.replayTurnPending;
            }
            stage = "input-snapshot";
            throwIfAborted(signal, "session/input/snapshot");
            const snapshot = await raceAbort(
                this.transport.request<QueueSnapshot>("session/input/snapshot", {
                    sessionId: this.id,
                }),
                signal,
                "session/input/snapshot",
            );
            const generation = nonEmpty(snapshot.generation);
            if (!generation) throw new Error("Peri returned no input queue generation");
            throwIfAborted(signal, "Session startup");
            this.queueGeneration = generation;
            this.state = "active";
            return {
                sessionId: this.id,
                path: request.path,
                generation,
                seed,
                historyBatch,
                history,
                queueSnapshot: snapshot,
                replayTurnPending,
            };
        } catch (error) {
            await this.failStartup(error, stage);
            throw error;
        }
    }

    private async failStartup(error: unknown, stage: SessionStartStage): Promise<never> {
        this.state = "cleanup-pending";
        this.deferredLive.length = 0;
        this.stagedReplay.length = 0;
        this.unsubscribe?.();
        this.unsubscribe = undefined;
        this.streamEnded = true;
        this.rejectAllDeliveries(asError(error));
        await this.notifications.return();
        if (error instanceof SessionStartError) throw error;
        throw new SessionStartError(
            stage,
            this.createdSessionId ?? null,
            `Session startup failed during ${stage}: ${errorMessage(error)}`,
            { cause: error },
        );
    }

    /** 初始化事实已发布；放行启动期间暂存的 live 通知（保持顺序）。 */
    beginLive(): void {
        if (this.live) return;
        this.live = true;
        const pending = this.deferredLive.splice(0);
        for (const event of pending) this.deliverState(event);
    }

    private receive(event: JsonRpcNotification): void {
        if (!event || !STATE_EVENTS.has(event.method)) return;
        const params = record(event.params);
        const eventSession = nonEmpty(params?.sessionId);
        const expected = this.sessionId ?? this.requestedSessionId;
        if (expected && eventSession && eventSession !== expected) return;
        const generation = loadGenerationOf(event);
        if (generation !== null) {
            // 历史回放：非本代次（迟到/被取代）直接丢弃；本代次在响应前只暂存。
            if (generation !== this.loadGeneration) return;
            if (RAW_EVENTS.has(event.method)) this.notifications.push(event);
            // 响应已安装整批回放后，同代次重复回放只影响文档，不进事实。
            if (this.replayInstalled) return;
            this.stagedReplay.push(event);
            return;
        }
        if (RAW_EVENTS.has(event.method)) this.notifications.push(event);
        this.noteReplayTurn(event);
        this.deliverState(event);
    }

    private noteReplayTurn(event: JsonRpcNotification): void {
        if (event.method === "peri/agent_event_done") {
            this.replayTurnPending = false;
            return;
        }
        if (event.method !== "session/update") return;
        const update = record(record(event.params)?.update);
        const replay = record(update?._meta)?.periReplay === true;
        if (!replay) return;
        if (REPLAY_KINDS.has(String(update?.sessionUpdate))) this.replayTurnPending = true;
    }

    private deliverState(event: JsonRpcNotification): void {
        if (!this.live) {
            if (this.deferredLive.length >= MAX_DEFERRED_LIVE) {
                console.error("[peri-sdk] Session live notification overflow during startup");
                return;
            }
            this.deferredLive.push(event);
            return;
        }
        if (event.method === "peri/agent_event") this.acceptDeliveryEvent(event);
        this.options.onNotification?.(event);
    }

    private acceptDeliveryEvent(event: JsonRpcNotification): void {
        if (event.method !== "peri/agent_event") return;
        const params = record(event.params);
        const decoded = decodeAgentEvent(params?.event_json);
        if (!decoded || decoded.type !== "user_input_delivered") return;
        const inputId = nonEmpty(decoded.value.input_id);
        if (!inputId) return;
        const generation = nonEmpty(decoded.value.generation);
        if (generation && this.queueGeneration && generation !== this.queueGeneration) return;
        this.confirmDelivery(inputId);
    }

    /** 当前绑定的队列 generation；翻页请求不得改变它。 */
    requestHistoryPage(cursor: string, limit: number | undefined): Promise<unknown> {
        return this.activeTransport().request("session/load", {
            sessionId: this.id,
            cursor,
            ...(limit === undefined ? {} : { limit }),
        });
    }

    /** 后台任务快照；任务 revision 缺口由运行侧检测后调用。 */
    requestBackgroundTasks(): Promise<unknown> {
        return this.activeTransport().request("session/bg-tasks", { sessionId: this.id });
    }

    private activeTransport(): Transport {
        if (this.closing || this.state !== "active")
            throw new Error("Session is not active");
        return this.transport;
    }

    private domainTransport(): Transport {
        return this.transport;
    }

    trackInput(inputId: string, text: string): void {
        this.pendingInputText.set(inputId, text);
    }

    waitForDelivery(inputId: string): Promise<void> {
        if (this.deliveredInputs.delete(inputId)) return Promise.resolve();
        const existing = this.pendingDeliveries.get(inputId);
        if (existing) return existing.promise;
        let resolve!: () => void;
        let reject!: (error: Error) => void;
        const promise = new Promise<void>((done, fail) => {
            resolve = done;
            reject = fail;
        });
        this.pendingDeliveries.set(inputId, { promise, resolve, reject });
        return promise;
    }

    confirmDelivery(inputId: string): void {
        const pending = this.pendingDeliveries.get(inputId);
        const text = this.pendingInputText.get(inputId);
        this.pendingInputText.delete(inputId);
        this.pendingDeliveries.delete(inputId);
        // 外部客户端的输入没有本地文本；事实仍要记录投递发生。
        this.options.onDelivered?.(inputId, text ?? "");
        if (pending) pending.resolve();
        else {
            this.deliveredInputs.add(inputId);
            while (this.deliveredInputs.size > 1024) {
                const oldest = this.deliveredInputs.values().next().value;
                if (oldest === undefined) break;
                this.deliveredInputs.delete(oldest);
            }
        }
    }

    rejectDelivery(inputId: string, error: unknown): void {
        this.pendingInputText.delete(inputId);
        this.deliveredInputs.delete(inputId);
        const pending = this.pendingDeliveries.get(inputId);
        if (!pending) return;
        this.pendingDeliveries.delete(inputId);
        pending.reject(asError(error));
    }

    private rejectAllDeliveries(error: Error): void {
        for (const inputId of [...this.pendingDeliveries.keys()]) this.rejectDelivery(inputId, error);
        this.pendingInputText.clear();
    }

    private queueReceiptState(receipt: QueueReceipt, inputId: string): string {
        const state = receipt.results?.find((item) => item.inputId === inputId)?.state;
        if (!state || state === "unknown" || state === "withdrawn")
            throw new Error(`Input not accepted: ${state ?? "missing"}`);
        if (receipt.snapshot !== undefined) this.options.onQueueSnapshot?.(receipt.snapshot);
        return state;
    }

    /** 入队即由 Rust mailbox 决定调度；SDK 不另行申请执行准入。 */
    async enqueue(inputId: string, text: string): Promise<string> {
        const receipt = await this.activeTransport().request<QueueReceipt>("session/input/enqueue", {
            sessionId: this.id,
            generation: this.queueGeneration,
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
            generation: this.queueGeneration,
            commandId: `${inputId}:dispatch`,
            inputIds: [inputId],
        });
        this.queueReceiptState(receipt, inputId);
    }

    /** 撤回队列中尚未派发的输入；`withdrawn` 是本次撤回成功的结果状态。 */
    async takeBack(inputId: string): Promise<void> {
        const receipt = await this.activeTransport().request<QueueReceipt>("session/input/takeback", {
            sessionId: this.id,
            generation: this.queueGeneration,
            commandId: `${inputId}:takeback`,
            inputId,
        });
        const state = receipt.results?.find((item) => item.inputId === inputId)?.state;
        if (receipt.snapshot !== undefined) this.options.onQueueSnapshot?.(receipt.snapshot);
        if (!state || state === "unknown")
            throw new Error(`Input not taken back: ${state ?? "missing"}`);
    }

    /** 当前 ACP 的取消是一次会话级通知；未完成 turn 的收敛由 Peri 决定。 */
    async cancel(): Promise<void> {
        await this.activeTransport().notify("session/cancel", { sessionId: this.id });
    }

    /** 写入会话配置；响应中的 configOptions 由 ACP 通知投影接管，这里只确认请求成功。 */
    async setConfigOption(configId: string, value: string): Promise<void> {
        await this.activeTransport().request("session/set_config_option", {
            sessionId: this.id,
            configId,
            value,
        });
    }

    /** 可选标题重命名；失败保留 `createdSessionId` 供上层判定创建阶段。 */
    async rename(title: string, options: { signal?: AbortSignal } = {}): Promise<void> {
        throwIfAborted(options.signal, "session/rename");
        await raceAbort(
            this.activeTransport().request("session/rename", { sessionId: this.id, title }),
            options.signal,
            "session/rename",
        );
    }

    /** 原始 ACP 通知：session/update 与 peri/agent_event。 */
    async *stream(): AsyncIterable<JsonRpcNotification> {
        if (!this.sessionId && !this.requestedSessionId) throw new Error("Session is not active");
        if (this.streamOpen)
            throw new Error("This Session already has an event consumer");
        this.streamOpen = true;
        try {
            while (!this.streamEnded) {
                const next = await this.notifications.next();
                if (next.done) break;
                const event = next.value;
                if (event && nonEmpty(record(event.params)?.sessionId) === (this.sessionId ?? null))
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
        if (this.state === "starting")
            return Promise.reject(new Error("Wait for Session startup to finish before close"));
        if (!this.closing) {
            this.closing = Promise.resolve()
                .then(() => this.closeOnce(options))
                .finally(() => { this.closing = undefined; });
            void this.closing.catch(() => {});
        }
        return this.closing;
    }

    private async closeOnce(options: CloseOptions): Promise<void> {
        const sessionId = this.sessionId;
        if (!sessionId) return this.finishClosed();
        // 关闭失败时保留 transport 与订阅，下一次调用重试同一个 session/close。
        await closeUntilSettled(
            () => this.domainTransport().request("session/close", { sessionId }),
            options,
        );
        this.state = "cleanup-pending";
        await this.finishClosed();
    }

    private async finishClosed(): Promise<void> {
        this.unsubscribe?.();
        this.unsubscribe = undefined;
        this.streamEnded = true;
        this.deferredLive.length = 0;
        this.stagedReplay.length = 0;
        this.rejectAllDeliveries(new Error("Session closed before user input was delivered"));
        await this.notifications.return();
        this.state = "closed";
    }

    cleanupStartup(): Promise<void> {
        if (this.sessionId)
            throw new Error("Startup cleanup cannot shut down an established Session");
        if (this.state === "starting")
            throw new Error("Wait for Session startup to finish before cleanupStartup");
        if (this.state === "active" || this.state === "closed")
            throw new Error(`Startup cleanup cannot run from ${this.state}`);
        if (!this.startupCleanupPromise) {
            this.startupCleanupPromise = this.finishClosed().finally(() => {
                this.startupCleanupPromise = undefined;
            });
        }
        return this.startupCleanupPromise;
    }
}

function asError(error: unknown): Error {
    return error instanceof Error ? error : new Error(String(error));
}

function errorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
}
