import { SessionOwnershipConflictError } from "./ownership";
import type { WasmRuntimeLease } from "../wasm/runtime-pool";
import {
    PayloadReadError,
    type ReadPayloadOptions,
    type ToolPayloadReadRef,
    type ToolPayloadResponse,
} from "../payload/reader";
import {
    parseChatUserAction,
    type CancelAction,
    type ChatUserAction,
    type ForceSendAction,
    type HistoryAction,
    type SendAction,
    type SessionAction,
    type TakeBackAction,
    type AnswerAction,
} from "./action";
import { AsyncMutex, ChatBinding, isChatIdle } from "./binding";
import { closeActivation } from "./close-chat";
import {
    ChatActionError,
    ChatBusyError,
    ChatClosedError,
    ChatNotReadyError,
    SessionBindingConflictError,
    SessionCreationError,
} from "./errors";
import { ChatFactHub } from "./fact-hub";
import type { ChatBindingTarget, ChatFact, ChatFactObserver } from "../contracts/chat";
import { readChatPayload } from "./payload-access";
import { ChatSendReceipt, InputCorrelation } from "./send-receipt";
import {
    SessionCandidate,
    resolveLoadPath,
    type CandidateContext,
} from "./service-candidates";
import { ChatCloseController } from "./service-lifecycle";
import {
    asActionError,
    bindingFailure,
    messageOf,
    withCleanupFailure,
    withRequestId,
} from "./error-mapping";
import { resolveChatLifecycle } from "./lifecycle";
import type {
    Activation,
    ChatInstanceFactory,
    ChatServiceOptions,
    CleanupOutcome,
} from "./service-context";
import type { HistoryPageOutcome, PeriInstancePort } from "../agent/port";
import type {
    CancelRequestResult,
    ChatLifecycleOptions,
    ChatService as ChatServiceContract,
    ControlResult,
    SendReceipt,
    SessionReadyResult,
} from "./types";

/**
 * 单个 Chat 的操作句柄。
 *
 * 只持有当前绑定、输入关联和事实日志；不持有 Manager，也不创建 View/Yjs。
 * `execute` 的 send 分支保持同步返回，其余分支在同一异步临界区内结算。
 */
export class ChatService implements ChatServiceContract {
    readonly id: string;

    private readonly path?: string;
    private readonly sessionInit: ChatServiceOptions["sessionInit"];
    private readonly agentConfig: unknown;
    private readonly runtime: ChatServiceOptions["runtime"];
    private readonly database: ChatServiceOptions["database"];
    private readonly ownership: ChatServiceOptions["ownership"];
    private readonly index: ChatServiceOptions["index"];
    private readonly instanceFactory: ChatInstanceFactory;
    private readonly acquireWasmRuntime: (
        signal?: AbortSignal,
    ) => Promise<WasmRuntimeLease>;
    private readonly onClosedHandler: (chatId: string) => void;
    private readonly lifecycleOptions: Required<ChatLifecycleOptions>;

    private readonly state: ChatBinding;
    private readonly mutex = new AsyncMutex();
    private readonly facts: ChatFactHub;
    private readonly inputs = new InputCorrelation();
    private readonly ownedSessions = new Set<string>();
    private readonly pendingTargets = new Map<
        string,
        Promise<ChatBindingTarget>
    >();
    private readonly pendingCleanups = new Set<Activation>();
    private readonly startupControllers = new Set<AbortController>();
    private readonly candidateContext: CandidateContext;
    private readonly closeController: ChatCloseController;

    private active?: Activation;
    private pendingAdmissions = 0;

    constructor(options: ChatServiceOptions) {
        this.id = options.id;
        this.state = new ChatBinding(options.id);
        this.facts = new ChatFactHub(options.id);
        this.path = options.path;
        this.sessionInit = options.sessionInit;
        this.agentConfig = options.agentConfig;
        this.runtime = options.runtime;
        this.database = options.database;
        this.ownership = options.ownership;
        this.index = options.index;
        this.instanceFactory = options.instanceFactory;
        this.acquireWasmRuntime = options.acquireWasmRuntime;
        this.onClosedHandler = options.onClosed;
        this.lifecycleOptions = resolveChatLifecycle(
            this.runtime.kind,
            options.lifecycle,
        );
        this.candidateContext = {
            chatId: this.id,
            database: this.database,
            path: this.path,
            sessionInit: this.sessionInit,
            index: this.index,
            acquireSession: (sessionId) => this.acquireSession(sessionId),
            releaseOwnership: (sessionId) => this.releaseOwnership(sessionId),
        };
        this.closeController = new ChatCloseController({
            id: this.id,
            mutex: this.mutex,
            state: this.state,
            facts: this.facts,
            lifecycle: this.lifecycleOptions,
            pendingCleanups: this.pendingCleanups,
            ownedSessions: this.ownedSessions,
            takeActive: () => {
                const active = this.active;
                this.active = undefined;
                return active;
            },
            releaseOwnership: (sessionId) => this.releaseOwnership(sessionId),
            releaseIndex: (sessionId) =>
                this.index.release(this.id, sessionId),
            onActivationClosed: (activation) =>
                this.startupControllers.delete(activation.controller),
            abortStartups: () => this.abortStartups(),
            failInputs: (makeError) => this.inputs.rejectAll(makeError),
            onClosed: (chatId) => this.onClosedHandler(chatId),
        });
    }

    private get closingRequested(): boolean {
        return this.closeController.closingRequested;
    }

    get sessionId(): string | null {
        return this.state.sessionId;
    }

    get bindingRevision(): number {
        return this.state.bindingRevision;
    }

    get binding(): ChatBindingTarget | null {
        return this.state.binding;
    }

    /** 仅供包内诊断/测试；不是公开控制面。 */
    get phase(): string {
        return this.state.phase;
    }

    execute(action: SendAction): SendReceipt;
    execute(action: SessionAction): Promise<SessionReadyResult>;
    execute(action: CancelAction): Promise<CancelRequestResult>;
    execute(
        action: ForceSendAction | TakeBackAction | AnswerAction,
    ): Promise<ControlResult>;
    execute(action: HistoryAction): Promise<HistoryPageOutcome>;
    execute(action: ChatUserAction): SendReceipt | Promise<
        | SessionReadyResult
        | CancelRequestResult
        | ControlResult
        | HistoryPageOutcome
    >;
    execute(action: ChatUserAction): SendReceipt | Promise<
        | SessionReadyResult
        | CancelRequestResult
        | ControlResult
        | HistoryPageOutcome
    > {
        // 不信任 instanceof/来源；HTTP/WS 与 SDK 内部使用同一套字段校验。
        const parsed = parseChatUserAction(action);
        switch (parsed.type) {
            case "send":
                return this.executeSend(parsed);
            case "create_new_session":
                return this.executeCreate(parsed);
            case "change_session":
                return this.executeChange(parsed);
            case "cancel":
                return this.executeCancel(parsed.requestId);
            case "force_send":
                return this.executeControl(
                    parsed.requestId,
                    "force_send",
                    (port) => port.dispatch(parsed.payload.inputId),
                );
            case "take_back":
                return this.executeTakeBack(
                    parsed.requestId,
                    parsed.payload.inputId,
                );
            case "answer":
                return this.executeControl(
                    parsed.requestId,
                    "answer",
                    (port) =>
                        port.answer(
                            parsed.payload.interactionId,
                            parsed.payload.answer,
                        ),
                );
            case "load_earlier_history":
                return this.executeHistory(parsed);
        }
    }

    async readPayload(
        ref: ToolPayloadReadRef,
        options?: ReadPayloadOptions,
    ): Promise<ToolPayloadResponse> {
        // 读取只对发起时的绑定代次有效。换绑/关闭后，旧端口的晚到结果与错误
        // 都不能交给调用方，否则调用方可能把旧会话的全文画进新绑定。
        const activation = this.active;
        const bindingRevision = this.state.bindingRevision;
        let outcome:
            | { response: ToolPayloadResponse }
            | { error: unknown };
        try {
            outcome = {
                response: await readChatPayload(activation?.port, ref, options),
            };
        } catch (error) {
            outcome = { error };
        }
        if (
            activation !== this.active ||
            bindingRevision !== this.state.bindingRevision
        ) {
            throw new PayloadReadError(
                "unavailable",
                "载荷读取完成前绑定已切换，晚到结果已丢弃",
                "error" in outcome ? { cause: outcome.error } : undefined,
            );
        }
        if ("error" in outcome) throw outcome.error;
        return outcome.response;
    }

    subscribeFacts(observer: ChatFactObserver): () => void {
        return this.facts.subscribe(observer);
    }

    close(): Promise<void> {
        return this.closeController.close();
    }

    // ------------------------------------------------------------------
    // Action execution
    // ------------------------------------------------------------------

    private executeSend(action: SendAction): SendReceipt {
        const { port, target } = this.requireReadyPort(action.requestId);
        const inputId = crypto.randomUUID();

        // 同步登记投递 waiter，再构造回执，最后才在微任务提交 enqueue。
        let delivery: Promise<void>;
        try {
            delivery = Promise.resolve(port.waitForDelivery(inputId));
        } catch (error) {
            throw asActionError(
                action.requestId,
                "delivery_wait_failed",
                error,
                "not_sent",
            );
        }

        this.pendingAdmissions += 1;
        let receipt: ChatSendReceipt;
        try {
            receipt = new ChatSendReceipt({
                requestId: action.requestId,
                inputId,
                target,
                delivery,
                send: async () => {
                    try {
                        await port.send(inputId, action.payload.text);
                    } finally {
                        this.pendingAdmissions = Math.max(
                            0,
                            this.pendingAdmissions - 1,
                        );
                    }
                },
            });
        } catch (error) {
            this.pendingAdmissions = Math.max(0, this.pendingAdmissions - 1);
            throw asActionError(
                action.requestId,
                "send_failed",
                error,
                "not_sent",
            );
        }

        this.inputs.register(inputId, { receipt, target });
        void receipt.then(
            () => this.inputs.settle(inputId),
            () => this.inputs.settle(inputId),
        );
        return receipt;
    }

    private executeCreate(
        action: Extract<ChatUserAction, { type: "create_new_session" }>,
    ): Promise<SessionReadyResult> {
        const { requestId } = action;
        this.assertOpen(requestId);
        const phase = this.state.phase;
        if (phase === "binding" || phase === "switching") {
            throw new ChatBusyError(
                requestId,
                `Chat is already ${phase} a session`,
            );
        }
        if (phase !== "empty") {
            throw new ChatNotReadyError(
                requestId,
                "create_new_session requires an empty chat handle",
            );
        }
        const path = this.path ?? this.sessionInit.path;
        if (!path) {
            throw new ChatNotReadyError(
                requestId,
                "A path is required to create a new session",
            );
        }
        this.state.beginCreate();
        return this.mutex
            .run(() =>
                this.createSession(requestId, path, action.payload.title),
            )
            .then((target) => ({ ...target, requestId }));
    }

    private executeChange(
        action: Extract<ChatUserAction, { type: "change_session" }>,
    ): Promise<SessionReadyResult> {
        const { requestId } = action;
        const sessionId = action.payload.sessionId;
        this.assertOpen(requestId);

        const current = this.state.binding;
        if (current?.sessionId === sessionId && this.state.phase === "ready") {
            return Promise.resolve({ ...current, requestId });
        }

        const pending = this.pendingTargets.get(sessionId);
        if (pending) {
            return pending.then(
                (target) => ({ ...target, requestId }),
                (error: unknown) => {
                    throw withRequestId(error, requestId);
                },
            );
        }

        const phase = this.state.phase;
        if (phase !== "empty" && phase !== "ready") {
            return Promise.reject(this.phaseError(requestId));
        }

        const operation = this.mutex.run(() =>
            this.loadSession(requestId, sessionId),
        );
        this.pendingTargets.set(sessionId, operation);
        const clear = (): void => {
            if (this.pendingTargets.get(sessionId) === operation)
                this.pendingTargets.delete(sessionId);
        };
        void operation.then(clear, clear);
        return operation.then(
            (target) => ({ ...target, requestId }),
            (error: unknown) => {
                throw withRequestId(error, requestId);
            },
        );
    }

    private async executeCancel(
        requestId: string,
    ): Promise<CancelRequestResult> {
        this.assertOpen(requestId);
        if (this.state.phase !== "ready" || !this.active) {
            return { requestId, status: "idle" };
        }
        try {
            const status = await this.active.port.cancel();
            return { requestId, status };
        } catch (error) {
            throw asActionError(
                requestId,
                "cancel_failed",
                error,
                "unknown",
            );
        }
    }

    private async executeControl(
        requestId: string,
        code: string,
        run: (port: PeriInstancePort) => Promise<void>,
    ): Promise<ControlResult> {
        this.assertOpen(requestId);
        const { port } = this.requireReadyPort(requestId);
        try {
            await run(port);
            return { requestId, status: "accepted" };
        } catch (error) {
            throw asActionError(requestId, `${code}_failed`, error, "unknown");
        }
    }

    private async executeTakeBack(
        requestId: string,
        inputId: string,
    ): Promise<ControlResult> {
        this.assertOpen(requestId);
        const { port } = this.requireReadyPort(requestId);
        try {
            await port.takeBack(inputId);
        } catch (error) {
            throw asActionError(
                requestId,
                "take_back_failed",
                error,
                "unknown",
            );
        }
        const pending = this.inputs.take(inputId);
        if (pending) {
            pending.receipt.reject(
                new ChatActionError(
                    pending.receipt.requestId,
                    "input_withdrawn",
                    "Input was taken back before delivery",
                    "rejected",
                ),
            );
        }
        return { requestId, status: "accepted" };
    }

    private async executeHistory(
        action: HistoryAction,
    ): Promise<HistoryPageOutcome> {
        const { requestId } = action;
        this.assertOpen(requestId);
        const { port } = this.requireReadyPort(requestId);
        try {
            return await port.loadEarlierHistory(action.payload.limit);
        } catch (error) {
            throw asActionError(
                requestId,
                "history_failed",
                error,
                "unknown",
            );
        }
    }

    // ------------------------------------------------------------------
    // Binding
    // ------------------------------------------------------------------

    private async createSession(
        requestId: string,
        path: string,
        title?: string,
    ): Promise<ChatBindingTarget> {
        let activation: Activation | undefined;
        let candidate: SessionCandidate | undefined;
        let createdSessionId: string | undefined;
        let stage: SessionCreationError["stage"] = "session/new";

        // session/new 返回后、rename 前登记索引与 ownership；不等待整个 bindNew 完成。
        const registerCreated = async (sessionId: string): Promise<void> => {
            if (createdSessionId !== undefined) {
                if (createdSessionId !== sessionId) {
                    throw new Error(
                        "PeriInstance.bindNew reported a different session id",
                    );
                }
                return;
            }
            createdSessionId = sessionId;
            stage = "claim";
            const next = new SessionCandidate(
                this.candidateContext,
                sessionId,
            );
            const owner = next.reserve();
            if (owner) {
                throw new SessionBindingConflictError(
                    requestId,
                    `Session ${sessionId} is already owned by chat ${owner}`,
                    owner,
                );
            }
            candidate = next;
            try {
                await next.acquire();
            } catch (error) {
                throw new SessionCreationError(
                    requestId,
                    sessionId,
                    "claim",
                    messageOf(error),
                    { cause: error },
                );
            }
            if (this.closingRequested) {
                throw new ChatClosedError(
                    requestId,
                    "Chat closed during session creation",
                );
            }
        };

        try {
            activation = this.createActivation(path);
            const result = await activation.port.bindNew({
                path,
                title,
                instructions: this.sessionInit.instructions,
                mcpServers: this.sessionInit.mcpServers,
                onSessionCreated: registerCreated,
            });
            const returned =
                typeof result?.sessionId === "string" &&
                result.sessionId.length > 0
                    ? result.sessionId
                    : undefined;
            if (returned !== undefined) {
                await registerCreated(returned);
            }
            if (createdSessionId === undefined) {
                throw new Error(
                    "PeriInstance.bindNew returned no session id",
                );
            }
            if (this.closingRequested) {
                throw new ChatClosedError(
                    requestId,
                    "Chat closed during session creation",
                );
            }
            return this.activate(activation, createdSessionId, null);
        } catch (error) {
            // PeriInstance 的阶段错误可能自带已创建的 Session；不依赖其具体类。
            const created = (error as { createdSessionId?: unknown })
                ?.createdSessionId;
            if (
                createdSessionId === undefined &&
                typeof created === "string" &&
                created.length > 0
            ) {
                createdSessionId = created;
            }
            const rawStage = (error as { stage?: unknown })?.stage;
            if (
                rawStage === "session/new" ||
                rawStage === "rename" ||
                rawStage === "input-snapshot"
            ) {
                stage = rawStage;
            }
            if (error instanceof SessionCreationError) {
                createdSessionId ??= error.createdSessionId;
                stage = error.stage;
            }
            const cleanup = await this.disposeActivation(activation);
            const release = candidate
                ? await candidate.release(cleanup)
                : {
                      released: cleanup.confirmed,
                      error: cleanup.error,
                  };
            if (this.closingRequested) this.state.beginClose();
            else if (release.released) this.state.restore("empty");
            else this.state.cleanupPending();

            if (createdSessionId !== undefined) {
                if (error instanceof SessionCreationError && release.released) {
                    throw withRequestId(error, requestId);
                }
                throw new SessionCreationError(
                    requestId,
                    createdSessionId,
                    stage,
                    messageOf(error),
                    {
                        cause: withCleanupFailure(error, release.error),
                    },
                );
            }
            throw bindingFailure(
                requestId,
                "session_creation_failed",
                error,
                release.released,
                release.error,
            );
        }
    }

    private async loadSession(
        requestId: string,
        sessionId: string,
    ): Promise<ChatBindingTarget> {
        const observedPhase = this.state.phase;
        if (observedPhase !== "empty" && observedPhase !== "ready") {
            throw this.phaseError(requestId);
        }
        const previousTarget = this.state.binding;
        if (
            previousTarget?.sessionId === sessionId &&
            observedPhase === "ready"
        ) {
            return previousTarget;
        }
        if (observedPhase === "ready" && this.active?.port.needsReload) {
            throw new ChatNotReadyError(
                requestId,
                "Chat runtime requires a reload before changing session",
            );
        }
        if (observedPhase === "ready" && !this.isIdleNow()) {
            throw new ChatBusyError(
                requestId,
                "Chat is busy; finish the current run, inputs and interactions before changing session",
            );
        }

        const previousPhase: "empty" | "ready" =
            observedPhase === "ready" ? "ready" : "empty";
        this.state.beginSwitch();
        // 索引必须在 path resolve 之前同步写入：path gate 期间 findChatBySession 必须命中。
        const candidate = new SessionCandidate(
            this.candidateContext,
            sessionId,
        );
        const owner = candidate.reserve();
        if (owner) {
            this.state.restore(previousPhase);
            throw new SessionBindingConflictError(
                requestId,
                `Session ${sessionId} is already owned by chat ${owner}`,
                owner,
            );
        }

        let activation: Activation | undefined;
        try {
            const path = await resolveLoadPath(
                this.candidateContext,
                sessionId,
                requestId,
            );
            if (this.closingRequested) {
                throw new ChatClosedError(
                    requestId,
                    "Chat closed while loading a session",
                );
            }
            try {
                await candidate.acquire();
            } catch (error) {
                if (error instanceof SessionOwnershipConflictError) {
                    throw new SessionBindingConflictError(
                        requestId,
                        `Session ${sessionId} is owned by another manager`,
                    );
                }
                throw asActionError(
                    requestId,
                    "claim_failed",
                    error,
                    "rejected",
                );
            }
            if (this.closingRequested) {
                throw new ChatClosedError(
                    requestId,
                    "Chat closed while loading a session",
                );
            }
            activation = this.createActivation(path);
            const result = await activation.port.bindExisting(sessionId);
            if (result?.sessionId !== sessionId) {
                throw new ChatActionError(
                    requestId,
                    "binding_failed",
                    "PeriInstance bound a different session",
                    "unknown",
                );
            }
            if (this.closingRequested) {
                throw new ChatClosedError(
                    requestId,
                    "Chat closed while loading a session",
                );
            }
        } catch (error) {
            const cleanup = await this.disposeActivation(activation);
            const release = await candidate.release(cleanup);
            if (this.closingRequested) this.state.beginClose();
            else if (release.released) this.state.restore(previousPhase);
            else this.state.cleanupPending();
            throw bindingFailure(
                requestId,
                "binding_failed",
                error,
                release.released,
                release.error,
            );
        }

        const previousSessionId = previousTarget?.sessionId ?? null;
        const old = this.active;
        if (old) {
            this.active = undefined;
            const closed = await closeActivation(
                old,
                this.lifecycleOptions.activationCloseTimeoutMs,
            );
            if (!closed.confirmed) {
                // 旧激活关闭未确认：保留旧资源与 ownership/索引，先完成候选清理再进入
                // blocked，绝不把超时当成已停止。
                this.pendingCleanups.add(old);
                const cleanup = await this.disposeActivation(activation);
                const release = await candidate.release(cleanup);
                this.state.block();
                throw bindingFailure(
                    requestId,
                    "binding_cleanup_failed",
                    closed.error,
                    false,
                    release.error,
                );
            }
            this.pendingCleanups.delete(old);
            this.startupControllers.delete(old.controller);
            if (previousSessionId !== null) {
                try {
                    await this.releaseOwnership(previousSessionId);
                } catch (error) {
                    const cleanup = await this.disposeActivation(activation);
                    const release = await candidate.release(cleanup);
                    this.state.block();
                    throw bindingFailure(
                        requestId,
                        "binding_cleanup_failed",
                        error,
                        release.released,
                        release.error,
                    );
                }
            }
        }

        if (this.closingRequested) {
            const cleanup = await this.disposeActivation(activation);
            const release = await candidate.release(cleanup);
            this.state.beginClose();
            throw new ChatClosedError(
                requestId,
                "Chat closed while loading a session",
                release.released ? undefined : { cause: release.error },
            );
        }

        return this.activate(activation!, sessionId, previousSessionId);
    }

    private async acquireSession(sessionId: string): Promise<void> {
        await this.ownership.acquire(sessionId);
        this.ownedSessions.add(sessionId);
    }

    private async releaseOwnership(sessionId: string): Promise<void> {
        await this.ownership.release(sessionId);
        this.ownedSessions.delete(sessionId);
    }

    private createActivation(path: string): Activation {
        const controller = new AbortController();
        this.startupControllers.add(controller);
        let port: PeriInstancePort;
        try {
            port = this.instanceFactory({
                chatId: this.id,
                path,
                agentConfig: this.agentConfig,
                sessionInit: this.sessionInit,
                runtime: this.runtime,
                signal: controller.signal,
                database: this.database,
                lifecycle: this.lifecycleOptions,
                acquireWasmRuntime: (signal) => this.acquireWasmRuntime(signal),
            });
        } catch (error) {
            this.startupControllers.delete(controller);
            throw error;
        }
        const activation: Activation = {
            port,
            controller,
            pending: [],
            detach: () => {},
            live: false,
            abandoned: false,
        };
        try {
            activation.detach = port.attach((fact) => {
                if (activation.abandoned) return;
                if (!activation.live) activation.pending.push(fact);
                else this.onLiveFact(activation, fact);
            });
        } catch (error) {
            this.startupControllers.delete(controller);
            controller.abort(error);
            void port.close().catch(() => {});
            throw error;
        }
        return activation;
    }

    private activate(
        activation: Activation,
        sessionId: string,
        previousSessionId: string | null,
    ): ChatBindingTarget {
        const target = this.state.commit(sessionId);
        activation.live = true;
        this.active = activation;
        this.index.commit(this.id, previousSessionId, sessionId);
        this.facts.bind(target, activation.pending);
        activation.pending.length = 0;
        return target;
    }

    private async disposeActivation(
        activation?: Activation,
    ): Promise<CleanupOutcome> {
        if (!activation) return { confirmed: true };
        const outcome = await closeActivation(
            activation,
            this.lifecycleOptions.activationCloseTimeoutMs,
        );
        if (outcome.confirmed) {
            this.pendingCleanups.delete(activation);
            this.startupControllers.delete(activation.controller);
        } else {
            this.pendingCleanups.add(activation);
        }
        return outcome;
    }

    private onLiveFact(activation: Activation, fact: ChatFact): void {
        if (activation !== this.active) return;
        if (fact.kind === "input-delivered") {
            this.inputs.markDelivered(fact.inputId);
        }
        this.facts.push(fact);
    }

    private isIdleNow(): boolean {
        const port = this.active?.port;
        return isChatIdle({
            running: port?.running ?? false,
            pendingInputs: Math.max(
                port?.pendingInputCount ?? 0,
                this.inputs.size,
            ),
            pendingInteractions: port?.pendingInteractionCount ?? 0,
            pendingAdmissions: this.pendingAdmissions,
            needsReload: port?.needsReload ?? false,
        });
    }

    private requireReadyPort(
        requestId: string,
    ): { port: PeriInstancePort; target: ChatBindingTarget } {
        this.assertOpen(requestId);
        const target = this.state.binding;
        const port = this.active?.port;
        if (this.state.phase !== "ready" || !target || !port) {
            throw new ChatNotReadyError(
                requestId,
                `Chat has no ready binding (phase ${this.state.phase})`,
            );
        }
        if (port.needsReload) {
            throw new ChatNotReadyError(
                requestId,
                "Chat runtime requires a reload",
            );
        }
        return { port, target };
    }

    private assertOpen(requestId: string): void {
        const phase = this.state.phase;
        if (
            phase === "closed" ||
            phase === "closing" ||
            this.closingRequested
        ) {
            throw new ChatClosedError(requestId, "Chat is closing or closed");
        }
        if (phase === "blocked" || phase === "cleanup-pending") {
            throw new ChatNotReadyError(
                requestId,
                "Chat cleanup is pending; reload is required",
            );
        }
    }

    private phaseError(requestId: string): ChatActionError {
        const phase = this.state.phase;
        if (phase === "binding" || phase === "switching") {
            return new ChatBusyError(
                requestId,
                `Chat is already ${phase} a session`,
            );
        }
        return new ChatNotReadyError(
            requestId,
            `Chat cannot accept this action in phase ${phase}`,
        );
    }

    private abortStartups(): void {
        for (const controller of this.startupControllers) {
            controller.abort(new Error("Chat is closing"));
        }
    }
}
