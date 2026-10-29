import type { ChatFact } from "../contracts/chat";
import type { HistoryPageOutcome, PeriInstancePort } from "./port";
import type { HistoryPageItem } from "../state/history-merge";
import { toolContentText } from "../state/protocol";
import { HistoryBrowser } from "../state/history-browser";
import { PayloadStore, PayloadStoreError, type PayloadField } from "../state/payload-store";
import { TaskGapDetector } from "../state/task-gap";
import { ToolPayloadCapacityError, type ToolPayloadBudgets } from "../state/tool-payloads";
import { PayloadReadError } from "../payload/reader";
import { AbortRaceError, DeadlineError, raceAbort, throwIfAborted, withDeadline } from "../util/abort";
import { AcpMcpBridge, resolveAcpMcpBindings, type AcpMcpBinding } from "./acp-mcp-bridge";
import { InteractionBroker } from "./interaction-broker";
import type { InteractionAnswer } from "../contracts/interaction";
import { Session, SessionStartError, type SessionStartResult } from "./session";
import type { CloseOptions } from "./session-close";
import type { AcpMcpServer } from "../contracts/acp";
import type { JsonRpcNotification, Transport } from "../transport/types";

/**
 * 一次激活的宿主 + Session 装配与运行事实输出。
 *
 * - 宿主租约/transport 由调用方提供并在关闭时校验 `confirmed`；失败保留资源并阻断新执行。
 * - 事实只经 [`ChatFact`] 单向输出；本类不持有 Y.Doc、不实现 AgentRuntime、不 claim。
 * - 无 View 时也能执行：所有事实由 sink 接收，未挂 sink 只是丢弃输出。
 */

/** 一次宿主激活：transport 与可重试的关闭证据。 */
export interface PeriHostActivation {
    readonly transport: Transport;
    close(): Promise<{ confirmed: boolean; error?: unknown }>;
}

export type PeriHostActivator = (
    path: string,
    options: { signal: AbortSignal },
) => Promise<PeriHostActivation>;

type PeriInstanceBindStage =
    | "mcp-connect" | "session/new" | "claim" | "session/load" | "rename" | "input-snapshot";

/** 绑定阶段失败；`createdSessionId` 已创建时上层不得盲目重复新建。 */
export class PeriInstanceBindError extends Error {
    constructor(
        readonly stage: PeriInstanceBindStage,
        readonly createdSessionId: string | null,
        message: string,
        options?: { cause?: unknown },
    ) {
        super(message, options);
        this.name = "PeriInstanceBindError";
    }
}

export class PeriInstanceNotBoundError extends Error {
    constructor(message = "Peri instance is not bound to a Session") {
        super(message);
        this.name = "PeriInstanceNotBoundError";
    }
}

/** 宿主清理未确认；资源保留，调用方可重试 close。 */
export class PeriHostCleanupError extends Error {
    constructor(message: string, options?: { cause?: unknown }) {
        super(message, options);
        this.name = "PeriHostCleanupError";
    }
}

/** 关闭未在预算内确认；资源保留，不产生 `closed` 事实，可第二次 close。 */
export class PeriInstanceCloseIncompleteError extends Error {
    readonly failures: readonly unknown[];

    constructor(
        failures: readonly unknown[] = [],
        message = "Peri instance close is incomplete; resources are preserved for retry",
        options?: { cause?: unknown },
    ) {
        super(message, {
            cause: options?.cause ?? (failures.length === 0
                ? undefined
                : failures.length === 1
                    ? failures[0]
                    : new AggregateError(failures, message)),
        });
        this.name = "PeriInstanceCloseIncompleteError";
        this.failures = failures;
    }
}

export interface PeriInstanceCloseOptions extends CloseOptions {
    /** 中止启动后等待绑定调用结算；默认 5000ms。 */
    bindingSettleTimeoutMs?: number;
    /** 单个激活（宿主/transport）关闭等待；默认 5000ms。 */
    activationCloseTimeoutMs?: number;
}

export interface PeriInstanceOptions {
    acquireHost: PeriHostActivator;
    /** 读取持久 cwd；`session/load` 只用它，不发送冻结 prompt 覆盖。 */
    resolveSessionPath: (sessionId: string) => Promise<string | null>;
    /** `session/load` 重发的 MCP 声明（待决项 D4 的默认选择）。 */
    defaultMcpServers?: Record<string, AcpMcpServer>;
    payloadBudgets?: ToolPayloadBudgets;
    /** 测试/确定性内容寻址用；生产默认每个绑定随机。 */
    payloadNonce?: string;
    /** 上层生命周期信号：只中止启动等待，不直接关闭健康 transport。 */
    signal?: AbortSignal;
    close?: PeriInstanceCloseOptions;
}

type InstanceState = "idle" | "binding" | "bound" | "blocked" | "closed";

type BindOptions = {
    path?: string;
    title?: string;
    instructions?: string;
    mcpServers?: Record<string, AcpMcpServer>;
    sessionId?: string;
    onSessionCreated?: (sessionId: string) => void | Promise<void>;
};

type TurnStatus = "completed" | "cancelled" | "error";

function record(value: unknown): Record<string, unknown> | null {
    return value !== null && typeof value === "object" && !Array.isArray(value)
        ? value as Record<string, unknown>
        : null;
}

function nonEmpty(value: unknown): string | null {
    return typeof value === "string" && value.length > 0 ? value : null;
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

/** 把声明转换成 ACP `session/new|load` 的 `mcpServers` 数组。 */
export function acpMcpServers(servers: Record<string, AcpMcpServer> | undefined): unknown[] {
    return Object.entries(servers ?? {}).map(([name, server]) => {
        if (server.type === "acp")
            return { name, type: "acp", serverId: server.serverId ?? name };
        if (server.type === "http")
            return {
                name,
                type: "http",
                url: server.url,
                headers: Object.entries(server.headers ?? {}).map(([header, value]) => ({
                    name: header, value,
                })),
            };
        return {
            name,
            type: "stdio",
            command: server.command,
            ...(server.args === undefined ? {} : { args: server.args }),
            env: Object.entries(server.env ?? {}).map(([envName, value]) => ({
                name: envName, value,
            })),
        };
    });
}

const QUEUE_STATES = new Set(["queued", "dispatching", "claimed", "delivered", "withdrawn"]);

export class PeriInstance implements PeriInstancePort {
    private readonly sinks = new Set<(fact: ChatFact) => void>();
    private readonly startupController = new AbortController();
    private removeExternalAbort?: () => void;
    private state: InstanceState = "idle";
    private activation: PeriHostActivation | null = null;
    private session: Session | null = null;
    private bridge: AcpMcpBridge | null = null;
    private broker: InteractionBroker | null = null;
    private payloads = new PayloadStore();
    private history = new HistoryBrowser();
    private readonly taskGap: TaskGapDetector;
    private sessionId: string | null = null;
    private bindingPromise: Promise<{ sessionId: string }> | null = null;
    private closing: Promise<void> | null = null;
    private needsReloadValue = false;
    private closedFactEmitted = false;
    private runningValue = false;
    private turnSerial = 0;
    private startedTurns = 0;
    private turnId: string | null = null;
    private activeRequestId: string | null = null;
    private activeUserMessageId: string | null = null;
    private readonly trackedInputs = new Map<string, string>();
    private readonly deliveredFacts = new Set<string>();
    private readonly seenEvents = new Set<string>();

    constructor(private readonly options: PeriInstanceOptions) {
        this.taskGap = new TaskGapDetector((snapshot) => {
            this.emit({ kind: "task-snapshot", snapshot });
        });
        const external = options.signal;
        if (external) {
            if (external.aborted) this.startupController.abort(external.reason);
            else {
                const relay = (): void => this.startupController.abort(external.reason);
                external.addEventListener("abort", relay, { once: true });
                this.removeExternalAbort = () => external.removeEventListener("abort", relay);
            }
        }
    }

    private get startupSignal(): AbortSignal { return this.startupController.signal; }

    /* ------------------------------- 观测面 ------------------------------- */

    get running(): boolean { return this.runningValue; }
    get pendingInputCount(): number { return this.trackedInputs.size; }
    get pendingInteractionCount(): number { return this.broker?.pendingCount ?? 0; }
    get needsReload(): boolean { return this.needsReloadValue; }
    get boundSessionId(): string | null { return this.sessionId; }

    /** 先同步挂 sink；晚挂载由 ChatService 的 FactLog 负责重放。 */
    attach(sink: (fact: ChatFact) => void): () => void {
        this.sinks.add(sink);
        if (this.closedFactEmitted) {
            try { sink({ kind: "closed" }); }
            catch (error) { console.error("[peri-sdk] Chat fact sink failed", error); }
        }
        return () => { this.sinks.delete(sink); };
    }

    /* -------------------------------- 绑定 -------------------------------- */

    bindNew(options: {
        path: string;
        title?: string;
        instructions?: string;
        mcpServers?: Record<string, AcpMcpServer>;
        onSessionCreated?: (sessionId: string) => void | Promise<void>;
    }): Promise<{ sessionId: string }> {
        return this.runBind({ ...options, sessionId: undefined }, "session/new");
    }

    async bindExisting(sessionId: string): Promise<{ sessionId: string }> {
        return this.runBind({ sessionId }, "session/load");
    }

    private runBind(options: BindOptions, stage: PeriInstanceBindStage): Promise<{ sessionId: string }> {
        this.assertBindable();
        this.state = "binding";
        const pending = this.doBind(options, stage);
        this.bindingPromise = pending;
        void pending.finally(() => { this.bindingPromise = null; }).catch(() => {});
        return pending;
    }

    private async doBind(options: BindOptions, bindStage: PeriInstanceBindStage): Promise<{ sessionId: string }> {
        let activation: PeriHostActivation | undefined;
        let session: Session | undefined;
        let bridge: AcpMcpBridge | undefined;
        let broker: InteractionBroker | undefined;
        try {
            throwIfAborted(this.startupSignal, "Peri session binding");
            const path = options.path ?? await this.options.resolveSessionPath(options.sessionId ?? "");
            if (!path)
                throw new PeriInstanceBindError("session/load", null,
                    `Session not found: ${options.sessionId ?? ""}`);
            activation = await this.acquireActivation(path);
            this.activation = activation;
            const servers = options.mcpServers ?? this.options.defaultMcpServers;
            let bindings: AcpMcpBinding[];
            try {
                bindings = await resolveAcpMcpBindings(servers);
            } catch (error) {
                throw new PeriInstanceBindError("mcp-connect", null,
                    `MCP server resolution failed: ${errorMessage(error)}`, { cause: error });
            }
            bridge = new AcpMcpBridge(bindings);
            this.bridge = bridge;
            broker = new InteractionBroker({
                emit: (fact) => this.emit(fact),
                currentTurnId: () => this.turnId,
            });
            this.broker = broker;
            this.payloads = new PayloadStore(this.options.payloadBudgets, this.options.payloadNonce);
            this.history = new HistoryBrowser();
            session = new Session(activation.transport, {
                requestHandler: (method, params) => method === "mcp/message"
                    ? bridge!.handle(params)
                    : broker!.handleRequest(method, params),
                onNotification: (notification) => this.acceptNotification(notification),
                onQueueSnapshot: (snapshot) => this.emit({ kind: "input-queue", snapshot }),
                onDelivered: (inputId, text) => this.handleDelivered(inputId, text),
                onBeforeSession: () => bridge!.connect(),
            });
            this.session = session;
            const started = await session.start({
                path,
                sessionId: options.sessionId ?? null,
                instructions: options.instructions,
                mcpServers: acpMcpServers(servers),
            }, {
                signal: this.startupSignal,
                ...(options.onSessionCreated ? { onSessionCreated: options.onSessionCreated } : {}),
            });
            const createdSessionId = started.sessionId;
            throwIfAborted(this.startupSignal, "Peri session binding");
            if (options.title !== undefined && options.title.length > 0) {
                try {
                    await session.rename(options.title, { signal: this.startupSignal });
                } catch (error) {
                    throw new PeriInstanceBindError("rename", createdSessionId,
                        `Session was created but rename failed: ${errorMessage(error)}`, { cause: error });
                }
            }
            throwIfAborted(this.startupSignal, "Peri session binding");
            broker.setSession(started.sessionId);
            this.commitBinding(started);
            return { sessionId: started.sessionId };
        } catch (error) {
            throw await this.failBinding(error, bindStage, activation, session, bridge, broker);
        }
    }

    /** 宿主工厂可能忽略取消；迟到 activation 必须关闭并留下证据。 */
    private async acquireActivation(path: string): Promise<PeriHostActivation> {
        throwIfAborted(this.startupSignal, "Peri host activation");
        const pending = this.options.acquireHost(path, { signal: this.startupSignal });
        try {
            return await raceAbort(pending, this.startupSignal, "Peri host activation");
        } catch (error) {
            if (error instanceof AbortRaceError) {
                void pending.then(async (late) => {
                    try {
                        const report = await late.close();
                        if (report.confirmed) {
                            console.error(
                                "[peri-sdk] Late host activation was closed after the startup was aborted",
                            );
                        } else {
                            console.error("[peri-sdk] Late host activation close was not confirmed",
                                report.error ?? "no reason");
                        }
                    } catch (closeError) {
                        console.error("[peri-sdk] Late host activation close failed", closeError);
                    }
                }, () => {});
            }
            throw error;
        }
    }

    private commitBinding(started: SessionStartResult): void {
        if (this.startupSignal.aborted)
            throw new Error("Peri instance startup was aborted before the binding committed");
        this.sessionId = started.sessionId;
        this.history.seed({
            snapshotToken: started.history.snapshotToken,
            nextCursor: started.history.nextCursor,
        });
        this.taskGap.setRequester(() => this.requireSession().requestBackgroundTasks());
        this.runningValue = false;
        this.state = "bound";
        this.needsReloadValue = false;
        this.emit({ kind: "seed-config", response: started.seed });
        if (started.historyBatch.length)
            this.emit({
                kind: "history-batch",
                notifications: started.historyBatch.map((notification) => this.normalizeNotification(notification)),
            });
        if (started.replayTurnPending) this.emit({ kind: "turn-complete", status: "completed" });
        this.emit({ kind: "input-queue", snapshot: started.queueSnapshot });
        this.emitRunState();
        this.session?.beginLive();
    }

    private async failBinding(
        error: unknown,
        bindStage: PeriInstanceBindStage,
        activation: PeriHostActivation | undefined,
        session: Session | undefined,
        bridge: AcpMcpBridge | undefined,
        broker: InteractionBroker | undefined,
    ): Promise<PeriInstanceBindError> {
        const failures: unknown[] = [];
        broker?.close();
        if (this.broker === broker) this.broker = null;
        if (session) {
            try {
                if (session.establishedSessionId) await session.close(this.options.close);
                else await session.cleanupStartup();
                if (this.session === session) this.session = null;
            } catch (cleanupError) {
                failures.push(cleanupError);
            }
        }
        // Session 仍可能存活时保留 bridge/host，供第二次 close 重试；只有会话已结算才释放下游。
        if (!session || this.session === null) {
            if (bridge) {
                try {
                    await bridge.close("Peri session binding failed");
                    if (this.bridge === bridge) this.bridge = null;
                } catch (closeError) {
                    failures.push(closeError);
                }
            }
            if (activation) {
                try {
                    const report = await activation.close();
                    if (report.confirmed) {
                        if (this.activation === activation) this.activation = null;
                    } else {
                        failures.push(report.error ?? new Error("Host close was not confirmed"));
                    }
                } catch (cleanupError) {
                    failures.push(cleanupError);
                }
            }
        }
        const stage = bindErrorStage(error, bindStage);
        const createdSessionId = bindErrorCreatedSessionId(error);
        if (failures.length > 0) {
            this.needsReloadValue = true;
            this.state = "blocked";
            // 资源保留、执行被阻断：让已挂 View 明确看到状态而不是假装已停止。
            this.emitRunState();
            return new PeriInstanceBindError(stage, createdSessionId,
                `Peri session binding failed; cleanup was not confirmed: ${errorMessage(error)}`, {
                    cause: new AggregateError([error, ...failures], "Peri session binding and cleanup failed"),
                });
        }
        this.state = "idle";
        this.needsReloadValue = false;
        if (error instanceof PeriInstanceBindError) return error;
        return new PeriInstanceBindError(stage, createdSessionId, errorMessage(error), { cause: error });
    }

    private assertBindable(): void {
        if (this.needsReloadValue)
            throw new Error("Peri instance cannot bind again after an unconfirmed cleanup");
        if (this.state === "binding") throw new Error("Peri instance is already binding a Session");
        if (this.state !== "idle")
            throw new Error(`Peri instance cannot bind from ${this.state}`);
    }

    /* ------------------------------ 输入与执行 ----------------------------- */

    async send(inputId: string, text: string): Promise<void> {
        const session = this.requireSession();
        this.trackedInputs.set(inputId, "queued");
        session.trackInput(inputId, text);
        try {
            const state = await session.enqueue(inputId, text);
            if (state === "delivered") session.confirmDelivery(inputId);
            if (this.trackedInputs.has(inputId)) this.trackedInputs.set(inputId, state);
        } catch (error) {
            this.trackedInputs.delete(inputId);
            throw error;
        }
    }

    waitForDelivery(inputId: string): Promise<void> {
        return this.requireSession().waitForDelivery(inputId);
    }

    async dispatch(inputId: string): Promise<void> {
        const session = this.requireSession();
        await session.dispatch(inputId);
        if (this.trackedInputs.has(inputId)) this.trackedInputs.set(inputId, "dispatching");
    }

    async takeBack(inputId: string): Promise<void> {
        const session = this.requireSession();
        await session.takeBack(inputId);
        this.trackedInputs.delete(inputId);
    }

    async cancel(): Promise<"requested" | "idle"> {
        const session = this.requireSession();
        if (!this.runningValue) return "idle";
        await session.cancel();
        this.emit({ kind: "turn-cancel", restore: false });
        return "requested";
    }

    async answer(interactionId: string, answer: InteractionAnswer): Promise<void> {
        this.requireSession();
        const broker = this.broker;
        if (!broker) throw new PeriInstanceNotBoundError();
        broker.answer(interactionId, answer);
    }

    async setConfigOption(configId: string, value: string): Promise<void> {
        await this.requireSession().setConfigOption(configId, value);
    }

    async loadEarlierHistory(limit?: number): Promise<HistoryPageOutcome> {
        const session = this.requireSession();
        const sessionId = this.requireSessionId();
        const page = await this.history.load(sessionId, limit,
            (_sessionId, cursor, pageLimit) => session.requestHistoryPage(cursor, pageLimit));
        this.emit({
            kind: "history-page",
            items: page.items.map((item) => this.normalizeHistoryItem(item)),
            nextCursor: page.nextCursor,
        });
        return { applied: page.applied, nextCursor: page.nextCursor };
    }

    /** 只读运行侧载荷仓库；引用 id、字节数、合法 JSON 与 `maxBytes` 都在这里校验。 */
    async readPayload(
        ref: { id: string; bytes: number },
        options: { signal?: AbortSignal; maxBytes?: number } = {},
    ): Promise<{ id: string; bytes: number; json: string }> {
        if (options.signal?.aborted)
            throw new PayloadReadError("aborted", "Payload read was aborted", { cause: options.signal.reason });
        if (this.state === "closed")
            throw new PayloadReadError("unavailable", "Peri instance is closed");
        let json: string;
        try {
            json = this.payloads.read(ref, { maxBytes: options.maxBytes });
        } catch (error) {
            if (error instanceof PayloadStoreError) {
                // 端口契约：非法 JSON 归为完整性错误，其余码一一映射。
                const code = error.code === "invalid_json" ? "integrity_error" : error.code;
                throw new PayloadReadError(code, error.message, { cause: error });
            }
            throw error;
        }
        return { id: ref.id, bytes: ref.bytes, json };
    }

    /* -------------------------------- 关闭 -------------------------------- */

    close(options: PeriInstanceCloseOptions = {}): Promise<void> {
        if (this.closedFactEmitted) return Promise.resolve();
        // 先中止可安全脱离的启动等待；不直接关闭健康 transport。
        if (!this.startupController.signal.aborted)
            this.startupController.abort(new Error("Peri instance is closing"));
        if (!this.closing) {
            const budgets = { ...this.options.close, ...options };
            this.closing = this.closeOnce(budgets).finally(() => { this.closing = null; });
            void this.closing.catch(() => {});
        }
        return this.closing;
    }

    private async closeOnce(options: PeriInstanceCloseOptions): Promise<void> {
        const bindingTimeout = closeBudget(options.bindingSettleTimeoutMs, 5_000, "bindingSettleTimeoutMs");
        const activationTimeout = closeBudget(options.activationCloseTimeoutMs, 5_000, "activationCloseTimeoutMs");
        if (this.bindingPromise) {
            try {
                await withDeadline(
                    this.bindingPromise.then(() => undefined),
                    bindingTimeout,
                    "Peri instance binding settle",
                );
            } catch (error) {
                if (error instanceof DeadlineError) {
                    this.markBlocked();
                    throw new PeriInstanceCloseIncompleteError([error],
                        "Peri instance binding did not settle before the close deadline");
                }
                // 绑定失败已经留下自己的清理证据；这里继续尝试关闭仍保留的资源。
            }
        }
        const session = this.session;
        const activation = this.activation;
        if (!session && !activation && !this.bridge) return this.finishClose();
        if (session) {
            try {
                if (session.establishedSessionId) await session.close(this.options.close);
                else await session.cleanupStartup();
                if (this.session === session) this.session = null;
            } catch (error) {
                // 关闭未确认：运行/资源状态保持原样，只标记执行阻断，不伪装已停止。
                this.markBlocked();
                throw error;
            }
        }
        // Session 已确认关闭：未决交互按安全默认失效，任务补读停止。
        this.broker?.close();
        this.broker = null;
        this.taskGap.reset();
        if (this.bridge) {
            try {
                await this.bridge.close("Peri session closed");
                this.bridge = null;
            } catch (error) {
                // 保留 bridge 供第二次 close 重试 owned 实例与 transport。
                this.markBlocked();
                throw new PeriInstanceCloseIncompleteError([error],
                    "MCP bridge close was not confirmed; bridge is retained for retry");
            }
        }
        if (activation) {
            let report: { confirmed: boolean; error?: unknown };
            try {
                report = await withDeadline(
                    activation.close(),
                    activationTimeout,
                    "Peri host activation close",
                );
            } catch (error) {
                if (error instanceof DeadlineError) {
                    this.markBlocked();
                    throw new PeriInstanceCloseIncompleteError([error],
                        "Peri host close did not settle before the close deadline; transport is retained for retry");
                }
                report = { confirmed: false, error };
            }
            if (!report.confirmed) {
                this.markBlocked();
                throw new PeriHostCleanupError("Host close was not confirmed; transport is retained for retry", {
                    cause: report.error,
                });
            }
            this.activation = null;
        }
        this.finishClose();
    }

    private markBlocked(): void {
        this.needsReloadValue = true;
        this.state = "blocked";
        this.emitRunState();
    }

    private finishClose(): void {
        this.state = "closed";
        this.needsReloadValue = false;
        this.runningValue = false;
        this.turnId = null;
        this.activeRequestId = null;
        this.trackedInputs.clear();
        this.history.reset();
        this.removeExternalAbort?.();
        this.removeExternalAbort = undefined;
        this.emitClosed();
    }

    private emitClosed(): void {
        if (this.closedFactEmitted) return;
        this.closedFactEmitted = true;
        this.emit({ kind: "closed" });
    }

    /* ------------------------------ 通知 → 事实 ---------------------------- */

    private acceptNotification(notification: JsonRpcNotification): void {
        if (this.state !== "bound") return;
        const params = record(notification.params);
        const eventId = nonEmpty(record(record(params?._meta)?.peri)?.eventId);
        if (eventId && this.seenEvents.has(eventId)) return;
        const normalized = this.normalizeNotification(notification);
        const update = notification.method === "session/update"
            ? record(normalized.params)?.update : undefined;
        // 新用户 turn 必须先发 turn-cancel：View 收到 notice 后会立刻把新 turn 设为 running，
        // 迟到的事实会误把新 turn 标成 cancelling。
        if (update) this.beginExternalUserTurn(update);
        this.emit({ kind: "notice", notification: normalized });
        if (eventId) {
            this.seenEvents.add(eventId);
            while (this.seenEvents.size > 4096) {
                const oldest = this.seenEvents.values().next().value;
                if (oldest === undefined) break;
                this.seenEvents.delete(oldest);
            }
        }
        if (notification.method === "session/update") return;
        if (notification.method === "peri/agent_event") {
            const event = decodeAgentEvent(params?.event_json);
            if (event) this.acceptAgentEvent(event.type, event.value);
            return;
        }
        if (notification.method === "peri/agent_event_done") {
            const requestId = nonEmpty(params?.requestId);
            if (!this.acceptsDone(requestId)) return;
            this.finishTurn(nonEmpty(params?.stopReason));
            return;
        }
        if (notification.method === "peri/unstable_event") {
            this.taskGap.observe(nonEmpty(params?.event), record(params?.data));
        }
    }

    private beginExternalUserTurn(raw: unknown): void {
        const update = record(raw);
        if (nonEmpty(update?.sessionUpdate) !== "user_message_chunk") return;
        if (record(update?._meta)?.periReplay === true) return;
        const messageId = nonEmpty(update?.messageId) ?? nonEmpty(record(update?._meta)?.periMessageId);
        if (messageId && messageId === this.activeUserMessageId) return;
        if (messageId) this.activeUserMessageId = messageId;
        if (!this.runningValue || messageId !== null) this.beginTurn();
    }

    private acceptAgentEvent(type: string, value: Record<string, unknown>): void {
        if (type === "user_input_run_started") {
            this.activeRequestId = nonEmpty(value.request_id);
            // 外部输入可能先以 user_message_chunk 开轮；同一轮不重复开新 turn。
            if (!this.runningValue) {
                this.activeUserMessageId = null;
                this.beginTurn();
            }
            return;
        }
        if (type === "user_input_queue_changed") {
            this.applyQueueSnapshot(value.snapshot);
            return;
        }
        if (type === "user_input_delivered") {
            // 本地输入的投递事实由 Session.onDelivered 产生；外部输入没有文本时在这里兜底。
            return;
        }
        if (type === "rewind_completed") {
            this.history.invalidate();
        }
    }

    private handleDelivered(inputId: string, text: string): void {
        const tracked = this.trackedInputs.delete(inputId);
        if (this.deliveredFacts.has(inputId)) return;
        this.deliveredFacts.add(inputId);
        while (this.deliveredFacts.size > 1024) {
            const oldest = this.deliveredFacts.values().next().value;
            if (oldest === undefined) break;
            this.deliveredFacts.delete(oldest);
        }
        // 外部客户端的输入没有本地文本：投递事实交给随后的 user_message_chunk 表达。
        if (!tracked && text.length === 0) return;
        // 正常顺序是 run_started → delivered；delivered 只在没有运行 turn 时兜底开轮。
        if (!this.runningValue) {
            this.activeUserMessageId = null;
            this.beginTurn();
        }
        this.emit({ kind: "input-delivered", inputId, text });
    }

    private applyQueueSnapshot(raw: unknown): void {
        const snapshot = record(raw);
        const generation = nonEmpty(snapshot?.generation);
        if (!generation || !Array.isArray(snapshot?.items)) return;
        this.trackedInputs.clear();
        for (const item of snapshot.items) {
            const value = record(item);
            const inputId = nonEmpty(value?.inputId);
            const state = nonEmpty(value?.state);
            if (!inputId || !state || !QUEUE_STATES.has(state)) continue;
            if (state !== "delivered" && state !== "withdrawn") this.trackedInputs.set(inputId, state);
        }
        this.emit({ kind: "input-queue", snapshot });
    }

    private beginTurn(): void {
        if (this.runningValue) this.emit({ kind: "turn-cancel", restore: false });
        this.turnSerial++;
        this.startedTurns++;
        this.turnId = `turn:${this.turnSerial}`;
        this.runningValue = true;
        this.emitRunState();
    }

    private acceptsDone(requestId: string | null): boolean {
        if (requestId && this.activeRequestId) return requestId === this.activeRequestId;
        if (requestId && !this.activeRequestId) return this.startedTurns <= 1;
        return true;
    }

    private finishTurn(stopReason: string | null): void {
        // 轮次结束是交互失效的权威边界；即使运行标记已收敛也要清空 pending。
        this.broker?.abortAll();
        if (!this.runningValue) return;
        const status: TurnStatus = stopReason === "cancelled" ? "cancelled"
            : stopReason === "max_tokens" || stopReason === "max_turn_requests" || stopReason === "error"
                ? "error" : "completed";
        this.runningValue = false;
        this.turnId = null;
        this.activeUserMessageId = null;
        this.activeRequestId = null;
        this.emit({ kind: "turn-complete", status, ...(stopReason ? { stopReason } : {}) });
        this.emitRunState();
    }

    private emitRunState(): void {
        this.emit({
            kind: "run-state",
            running: this.runningValue,
            executionBlocked: this.needsReloadValue,
        });
    }

    /* ------------------------------ 载荷归一化 ----------------------------- */

    private normalizeNotification(notification: JsonRpcNotification): JsonRpcNotification {
        if (notification.method === "peri/agent_event")
            return this.normalizeRewindNotification(notification);
        if (notification.method !== "session/update") return notification;
        const params = record(notification.params);
        const update = record(params?.update);
        if (!params || !update) return notification;
        const kind = nonEmpty(update.sessionUpdate);
        if (kind !== "tool_call" && kind !== "tool_call_update") return notification;
        const normalized = this.normalizeUpdate(update);
        return normalized === update ? notification : { ...notification, params: { ...params, update: normalized } };
    }

    /**
     * rewind 的 `messages_json` 是唯一绕过实时归一化的工具参数入口。
     *
     * 发 `notice` 之前把大参数换成 `argumentsRef`；超预算写 `argumentsError`；非法 JSON
     * 保留原事件并记录原因（View 仍会拒绝，不伪造空历史）。
     */
    private normalizeRewindNotification(notification: JsonRpcNotification): JsonRpcNotification {
        const params = record(notification.params);
        const event = decodeAgentEvent(params?.event_json);
        if (!event || event.type !== "rewind_completed") return notification;
        const messagesJson = event.value.messages_json;
        if (typeof messagesJson !== "string") return notification;
        let parsed: unknown;
        try {
            parsed = JSON.parse(messagesJson);
        } catch (error) {
            console.error("[peri-sdk] rewind_completed.messages_json is not valid JSON", error);
            return notification;
        }
        if (!Array.isArray(parsed)) {
            console.error("[peri-sdk] rewind_completed.messages_json is not a message array");
            return notification;
        }
        let changed = false;
        for (const rawMessage of parsed) {
            const message = record(rawMessage);
            if (!message) continue;
            if (Array.isArray(message.content)) {
                for (const rawBlock of message.content) {
                    const block = record(rawBlock);
                    if (!block || block.type !== "tool_use") continue;
                    changed = this.normalizeSnapshotArguments(block, "input") || changed;
                }
            }
            if (Array.isArray(message.tool_calls)) {
                for (const rawCall of message.tool_calls) {
                    const call = record(rawCall);
                    if (!call) continue;
                    changed = this.normalizeSnapshotArguments(call, "arguments") || changed;
                }
            }
        }
        if (!changed) return notification;
        const nextEvent = {
            type: event.type,
            value: { ...event.value, messages_json: JSON.stringify(parsed) },
        };
        return {
            ...notification,
            params: { ...params, event_json: JSON.stringify(nextEvent) },
        };
    }

    /** 与实时 tool_call 共用 PayloadStore 预算；引用存在时删除内联值。 */
    private normalizeSnapshotArguments(
        target: Record<string, unknown>,
        field: "input" | "arguments",
    ): boolean {
        if (target.argumentsRef !== undefined) {
            if (target[field] === undefined) return false;
            delete target[field];
            delete target.argumentsError;
            return true;
        }
        const value = target[field];
        if (value === undefined) return false;
        try {
            const normalized = this.payloads.normalize("arguments", value);
            if (normalized.kind === "inline") {
                target[field] = normalized.value;
                delete target.argumentsError;
                return false;
            }
            delete target[field];
            delete target.argumentsError;
            target.argumentsRef = normalized.ref;
            return true;
        } catch (error) {
            if (error instanceof ToolPayloadCapacityError) {
                // 不静默丢数据：公开错误随快照进入投影，原文不写入文档。
                delete target[field];
                delete target.argumentsRef;
                target.argumentsError = "payload_too_large";
                return true;
            }
            console.error("[peri-sdk] rewind tool payload normalization failed", error);
            return false;
        }
    }

    private normalizeUpdate(update: Record<string, unknown>): Record<string, unknown> {
        const next: Record<string, unknown> = { ...update };
        const rejected: PayloadField[] = [];
        if (update.rawInput !== undefined) {
            this.applyPayload(next, "arguments", update.rawInput, rejected);
            delete next.rawInput;
        }
        const status = nonEmpty(update.status);
        if (status === "completed" || status === "error") {
            const contentText = toolContentText(update.content);
            const result: Record<string, unknown> = {};
            if (contentText !== null)
                result.contentText = status === "error" && !contentText.trim() ? "Tool execution failed" : contentText;
            else if (status === "error") result.contentText = "Tool execution failed";
            if (update.rawOutput !== undefined) result.rawOutput = update.rawOutput;
            if (Object.keys(result).length > 0) this.applyPayload(next, "result", result, rejected);
            delete next.rawOutput;
            delete next.content;
        }
        if (rejected.length > 0 && next.error === undefined) {
            // View 的归一化契约：容量失败以公开错误随工具更新下发，不携带原文。
            next.error = { code: "payload_too_large", message: "工具载荷超出会话接纳预算" };
        }
        return next;
    }

    private applyPayload(
        update: Record<string, unknown>,
        field: PayloadField,
        value: unknown,
        rejected: PayloadField[],
    ): void {
        try {
            const normalized = this.payloads.normalize(field, value);
            if (normalized.kind === "inline") update[field] = normalized.value;
            else update[`${field}Ref`] = normalized.ref;
        } catch (error) {
            if (error instanceof ToolPayloadCapacityError) {
                // 超预算：事实不携带该字段，View 按公开错误展示；旧版本仍可读。
                rejected.push(field);
                return;
            }
            console.error("[peri-sdk] Tool payload normalization failed", error);
            update[field] = value;
        }
    }

    private normalizeHistoryItem(item: HistoryPageItem): HistoryPageItem {
        return {
            ...item,
            subItems: item.subItems.map((sub) => ({ ...sub, update: this.normalizeUpdate(sub.update) })),
        };
    }

    /* -------------------------------- 内部 -------------------------------- */

    private requireSession(): Session {
        if (this.state !== "bound" || !this.session || this.needsReloadValue)
            throw new PeriInstanceNotBoundError(this.needsReloadValue
                ? "Peri instance needs reload after an unconfirmed cleanup"
                : undefined);
        return this.session;
    }

    private requireSessionId(): string {
        if (!this.sessionId) throw new PeriInstanceNotBoundError();
        return this.sessionId;
    }

    private emit(fact: ChatFact): void {
        for (const sink of [...this.sinks]) {
            try { sink(fact); }
            catch (error) { console.error("[peri-sdk] Chat fact sink failed", error); }
        }
    }
}

function bindErrorStage(error: unknown, fallback: PeriInstanceBindStage): PeriInstanceBindStage {
    if (error instanceof PeriInstanceBindError) return error.stage;
    if (error instanceof SessionStartError) {
        if (error.stage === "mcp-connect") return "mcp-connect";
        if (error.stage === "claim") return "claim";
        if (error.stage === "session/load") return "session/load";
        if (error.stage === "input-snapshot") return "input-snapshot";
        return fallback === "session/load" ? "session/load" : "session/new";
    }
    return fallback;
}

function closeBudget(value: number | undefined, fallback: number, name: string): number {
    if (value === undefined) return fallback;
    if (!Number.isSafeInteger(value) || value < 0)
        throw new TypeError(`${name} must be a nonnegative safe integer`);
    return value;
}

function bindErrorCreatedSessionId(error: unknown): string | null {
    if (error instanceof PeriInstanceBindError) return error.createdSessionId;
    if (error instanceof SessionStartError) return error.createdSessionId;
    return null;
}

function errorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
}
