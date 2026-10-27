import { KvSessionOwnership, LocalSessionIndex } from "./ownership";
import {
    WasmRuntimePool,
    type WasmRuntimeAcquireOptions,
    type WasmRuntimeLease,
    type WasmRuntimeStarter,
} from "../wasm/runtime-pool";
import { startWasmRuntime, type WasmRuntimeDiagnosticKind } from "../wasm/runtime";
import { withDeadline } from "../util/deadline";
import type { ChatDatabase, ChatSessionDirectoryEntry } from "../contracts/chat";
import type {
    ChatSessionInit,
    ChatRuntime,
    ChatLifecycleOptions,
    ChatCreateOptions,
    ChatManager as ChatManagerContract,
    ChatManagerOptions,
    ChatStoreConfig,
    SessionOwnership,
} from "./types";
import { ChatService } from "./service";
import { createChatInstance } from "./instance-factory";
import {
    resolveChatAgentConfig,
    snapshotChatStore,
} from "./agent-config";
import { mergeChatLifecycle, resolveChatLifecycle } from "./lifecycle";
import { copyChatSessionInit } from "./session-init";
import type { ChatInstanceFactory, ChatSessionIndex } from "./service-context";

/**
 * Phase 4 接入真实 PeriInstance 的内部入口。
 *
 * 公开的 `ChatManagerOptions` 不含这些字段：Manager 默认装配真实工厂；
 * 测试可注入 fake port 或替代的 WASM 启动器。
 */
export interface ChatManagerInternalOptions extends ChatManagerOptions {
    readonly instanceFactory?: ChatInstanceFactory;
    /** 测试接缝：替代默认 `startWasmRuntime`，仍然接收 per-chat 启动配置。 */
    readonly wasmRuntimeStarter?: WasmRuntimeStarter;
}

/**
 * 多个 Chat 句柄的目录、占用协调与资源释放。
 *
 * 不接收普通执行动作，不决定 UI 选中项；`createChat` 只同步登记句柄，
 * 不启动 ACP、不取得 Session ownership、不读取历史。
 *
 * 这是 SDK 的默认实现；宿主编排层应依赖 `ChatManagerContract` 端口注入，
 * 不要把自己的组合规则写进本类。
 */
export class ChatManager implements ChatManagerContract {
    private readonly chats = new Map<string, ChatService>();
    private readonly sessions = new LocalSessionIndex();
    private readonly index: ChatSessionIndex;

    private readonly store: ChatStoreConfig;
    private readonly database: ChatDatabase;
    private readonly sessionOwnership: SessionOwnership;
    private readonly runtime: ChatRuntime;
    private readonly defaultSessionInit?: ChatSessionInit;
    private readonly instanceFactory: ChatInstanceFactory;
    private readonly wasmRuntimes?: WasmRuntimePool;
    private readonly lifecycle: Required<ChatLifecycleOptions>;

    private accepting = true;
    private shutdownPromise?: Promise<void>;
    private shutdownComplete = false;

    constructor(options: ChatManagerInternalOptions) {
        if (!options || typeof options !== "object") {
            throw new TypeError("ChatManager options are required");
        }
        if (
            !options.database ||
            typeof options.database.listSessions !== "function" ||
            typeof options.database.getSession !== "function"
        ) {
            throw new TypeError(
                "ChatManager requires a ChatDatabase with listSessions/getSession",
            );
        }
        // 调用方只注入存储端口；每个 Manager 自建 ownership，owner 身份不共享。
        const sessionOwnership = new KvSessionOwnership(
            options.kv,
        );
        if (
            !options.chatConfig ||
            typeof options.chatConfig !== "object" ||
            !options.chatConfig.runtime ||
            (options.chatConfig.runtime.kind !== "wasm" &&
                options.chatConfig.runtime.kind !== "transport")
        ) {
            throw new TypeError(
                'ChatManager requires chatConfig.runtime of { kind: "wasm" } or { kind: "transport" }',
            );
        }

        // Manager 是部署边界：Store 与机器身份只在这里声明并快照。
        this.store = snapshotChatStore(options.store);
        this.database = options.database;
        this.sessionOwnership = sessionOwnership;
        // 运行后端配置在构造时快照；env 也复制，避免调用方后续修改影响所有 Chat。
        const configuredRuntime = { ...options.chatConfig.runtime };
        this.runtime = configuredRuntime.kind === "wasm" &&
            configuredRuntime.env !== undefined
            ? { ...configuredRuntime, env: { ...configuredRuntime.env } }
            : configuredRuntime;
        // Manager 级 session 只是默认值；未配置时不伪造空配置对象。
        this.defaultSessionInit = options.chatConfig.session === undefined
            ? undefined
            : copyChatSessionInit(options.chatConfig.session);
        this.lifecycle = resolveChatLifecycle(
            this.runtime.kind,
            options.chatConfig.lifecycle,
        );
        // 默认工厂直接使用 per-chat context；生命周期预算已由 ChatService 合并。
        this.instanceFactory = options.instanceFactory ?? createChatInstance;
        this.index = {
            reserve: (chatId, sessionId) =>
                this.sessions.reserve(sessionId, chatId),
            commit: (chatId, previousSessionId, sessionId) =>
                this.sessions.commit(
                    chatId,
                    previousSessionId,
                    sessionId,
                ),
            release: (chatId, sessionId) =>
                this.sessions.release(sessionId, chatId),
        };

        if (this.runtime.kind === "wasm") {
            const runtime = this.runtime;
            const starter: WasmRuntimeStarter = options.wasmRuntimeStarter
                ?? (({ signal, configJson, env, onDiagnostic }) => {
                    if (configJson === undefined) {
                        throw new Error(
                            "WASM runtime requires a per-chat agentConfig snapshot",
                        );
                    }
                    return startWasmRuntime({
                        // 每个 Chat 传入自己的 configJson；Manager 不再持有全局配置。
                        configJson,
                        moduleUrl: runtime.moduleUrl,
                        moduleFactory: runtime.moduleFactory,
                        env: env ?? runtime.env,
                        signal,
                        onDiagnostic: onDiagnostic ?? runtime.onDiagnostic,
                    });
                });
            this.wasmRuntimes = new WasmRuntimePool(starter);
        }
    }

    /** 同步声明句柄；不启动 ACP、不取得 Session ownership。 */
    createChat(options: ChatCreateOptions): ChatService {
        if (!this.accepting) {
            throw new Error("ChatManager is shut down; no new chat handles");
        }
        if (!options || typeof options !== "object") {
            throw new TypeError("createChat requires an options object");
        }
        // per-chat agent 必填；Manager 只注入 store，不做默认合并。
        const { agentConfig, agentConfigJson } = resolveChatAgentConfig(
            this.store,
            options.agent,
        );
        // 句柄级 session 整体覆盖默认值；每个 Chat 持有独立嵌套容器。
        const sessionInit = copyChatSessionInit(
            options.session ?? this.defaultSessionInit,
        );
        const lifecycle = mergeChatLifecycle(
            this.lifecycle,
            this.runtime.kind,
            options.lifecycle,
        );
        const perChatRuntime = this.resolvePerChatRuntime(options.runtime);
        const payloadBudgets = options.payloadBudgets === undefined
            ? undefined
            : { ...options.payloadBudgets };
        const id = crypto.randomUUID();
        const chat = new ChatService({
            id,
            path: options.path,
            sessionInit,
            agentConfig,
            agentConfigJson,
            runtime: perChatRuntime.runtime,
            runtimeEnv: perChatRuntime.env,
            onRuntimeDiagnostic: perChatRuntime.onDiagnostic,
            payloadBudgets,
            database: this.database,
            ownership: this.sessionOwnership,
            index: this.index,
            instanceFactory: this.instanceFactory,
            lifecycle,
            acquireWasmRuntime: (acquireOptions) =>
                this.acquireWasmRuntime(acquireOptions),
            onClosed: (chatId) => this.removeChat(chatId),
        });
        this.chats.set(id, chat);
        return chat;
    }

    getChat(id: string): ChatService | undefined {
        return this.chats.get(id);
    }

    /** 覆盖 loading/bound/cleaning；清理确认后条目才移除。 */
    findChatBySession(sessionId: string): ChatService | undefined {
        const owner = this.sessions.ownerOf(sessionId);
        return owner === undefined ? undefined : this.chats.get(owner);
    }

    /** 只读取目录；不启动任何历史运行实例。 */
    listSessions(
        options?: { path?: string },
    ): Promise<ChatSessionDirectoryEntry[]> {
        return this.database.listSessions(options?.path);
    }

    /** 成功清理后才移除；未知/已关闭 id 为幂等 no-op。 */
    async closeChat(id: string): Promise<void> {
        const chat = this.chats.get(id);
        if (!chat) return;
        await chat.close();
    }

    /**
     * 先封闭新准入，再结算在途绑定与关闭，最后关闭 WASM 运行后端。
     * 失败聚合上报并保留句柄/ownership；可再次调用重试。
     */
    shutdown(): Promise<void> {
        this.accepting = false;
        if (this.shutdownComplete) return Promise.resolve();
        if (this.shutdownPromise) return this.shutdownPromise;
        const attempt = this.finalizeShutdown();
        this.shutdownPromise = attempt;
        void attempt.then(
            () => {
                this.shutdownPromise = undefined;
                this.shutdownComplete = true;
            },
            () => {
                this.shutdownPromise = undefined;
            },
        );
        return attempt;
    }

    private async finalizeShutdown(): Promise<void> {
        const failures: unknown[] = [];
        // 每个 chat.close 都有独立预算；超时是 Incomplete，不是停止证明。
        const results = await Promise.allSettled(
            [...this.chats.values()].map((chat) =>
                withDeadline(
                    chat.close(),
                    this.lifecycle.shutdownChatTimeoutMs,
                    `Chat ${chat.id} close`,
                ),
            ),
        );
        for (const result of results) {
            if (result.status === "rejected") failures.push(result.reason);
        }

        // 任一 Chat 未确认关闭时保留运行后端：其可能仍被未释放的激活使用。
        if (failures.length === 0 && this.wasmRuntimes) {
            try {
                const reports = await this.wasmRuntimes.closeAll();
                for (const report of reports) {
                    if (report.confirmed) continue;
                    failures.push(
                        new Error(
                            `Peri WASM runtime ${report.generationId} close is unconfirmed`,
                            { cause: report.error },
                        ),
                    );
                }
            } catch (error) {
                failures.push(error);
            }
        }

        if (failures.length > 0) {
            throw new AggregateError(
                failures,
                "ChatManager shutdown is incomplete",
            );
        }
        this.sessions.clear();
    }

    private resolvePerChatRuntime(
        override?: ChatCreateOptions["runtime"],
    ): {
        runtime: ChatRuntime;
        env?: Readonly<Record<string, string>>;
        onDiagnostic?: (kind: WasmRuntimeDiagnosticKind, error: unknown) => void;
    } {
        if (this.runtime.kind !== "wasm") return { runtime: this.runtime };
        const baseEnv = this.runtime.env;
        const env = override?.env === undefined
            ? (baseEnv === undefined ? undefined : { ...baseEnv })
            : { ...(baseEnv ?? {}), ...override.env };
        const onDiagnostic = override?.onDiagnostic ?? this.runtime.onDiagnostic;
        const runtime: ChatRuntime = {
            ...this.runtime,
            ...(env === undefined ? {} : { env }),
            ...(onDiagnostic === undefined ? {} : { onDiagnostic }),
        };
        return { runtime, env, onDiagnostic };
    }

    private acquireWasmRuntime(
        options?: WasmRuntimeAcquireOptions,
    ): Promise<WasmRuntimeLease> {
        if (!this.wasmRuntimes) {
            throw new Error(
                'runtime.kind "transport" does not use a WASM runtime pool',
            );
        }
        return this.wasmRuntimes.acquire(options);
    }

    private removeChat(id: string): void {
        this.chats.delete(id);
        this.sessions.removeChat(id);
    }
}
