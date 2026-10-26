import { cloneJsonValue } from "./json-value";
import {
    LocalSessionIndex,
    requireSessionOwnership,
} from "./ownership";
import {
    WasmRuntimePool,
    type WasmRuntimeLease,
} from "../wasm/runtime-pool";
import { startWasmRuntime } from "../wasm/runtime";
import { withDeadline } from "../util/deadline";
import type {
    ChatDatabase,
    ChatDefaults,
    ChatRuntime,
    ChatLifecycleOptions,
    ChatManager as ChatManagerContract,
    ChatManagerOptions,
    ChatSessionDirectoryEntry,
    SessionOwnership,
} from "./types";
import { ChatService } from "./service";
import { createChatInstance } from "./instance-factory";
import {
    resolveChatLifecycle,
    type ChatInstanceFactory,
    type ChatSessionIndex,
} from "./service-support";

/**
 * Phase 4 接入真实 PeriInstance 的内部入口。
 *
 * 公开的 `ChatManagerOptions` 不含该字段：Manager 默认装配真实工厂；
 * 测试可通过该字段注入 fake port。
 */
export interface ChatManagerInternalOptions extends ChatManagerOptions {
    readonly instanceFactory?: ChatInstanceFactory;
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

    private readonly agentConfig: unknown;
    private readonly agentConfigJson: string;
    private readonly database: ChatDatabase;
    private readonly sessionOwnership: SessionOwnership;
    private readonly runtime: ChatRuntime;
    private readonly defaults: ChatDefaults;
    private readonly instanceFactory: ChatInstanceFactory;
    private readonly now?: () => string;
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
        requireSessionOwnership(options.sessionOwnership);
        if (
            !options.runtime ||
            (options.runtime.kind !== "wasm" &&
                options.runtime.kind !== "transport")
        ) {
            throw new TypeError(
                'ChatManager runtime must be { kind: "wasm" } or { kind: "transport" }',
            );
        }

        this.agentConfig = cloneJsonValue(options.agentConfig, "agentConfig");
        this.agentConfigJson = JSON.stringify(this.agentConfig);
        this.database = options.database;
        this.sessionOwnership = options.sessionOwnership;
        // 运行后端配置在构造时快照；端口/模块工厂等实例仍由外部借用。
        this.runtime = { ...options.runtime };
        this.defaults = { ...options.defaults };
        this.now = options.now;
        this.lifecycle = resolveChatLifecycle(options.lifecycle);
        // 默认工厂只绑定 Manager 的生命周期预算；测试仍可注入 fake port。
        const lifecycle = this.lifecycle;
        this.instanceFactory = options.instanceFactory
            ?? ((context) => createChatInstance({ ...context, lifecycle }));
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
            this.wasmRuntimes = new WasmRuntimePool(
                ({ signal }) =>
                    startWasmRuntime({
                        configJson: this.agentConfigJson,
                        moduleUrl: runtime.moduleUrl,
                        moduleFactory: runtime.moduleFactory,
                        env: runtime.env,
                        moduleOptions: runtime.moduleOptions,
                        ports: runtime.ports,
                        signal,
                        cleanupTimeoutMs: runtime.cleanupTimeoutMs,
                        onLifecycle: runtime.onLifecycle,
                        onDiagnostic: runtime.onDiagnostic,
                    }),
                {
                    startupTimeoutMs: runtime.startupTimeoutMs,
                    closeTimeoutMs: runtime.cleanupTimeoutMs,
                },
            );
        }
    }

    /** 同步声明句柄；不启动 ACP、不取得 Session ownership。 */
    createChat(options?: { path?: string }): ChatService {
        if (!this.accepting) {
            throw new Error("ChatManager is shut down; no new chat handles");
        }
        const id = crypto.randomUUID();
        const chat = new ChatService({
            id,
            path: options?.path,
            defaults: this.defaults,
            agentConfig: this.agentConfig,
            runtime: this.runtime,
            database: this.database,
            ownership: this.sessionOwnership,
            index: this.index,
            instanceFactory: this.instanceFactory,
            lifecycle: this.lifecycle,
            acquireWasmRuntime: (signal) => this.acquireWasmRuntime(signal),
            onClosed: (chatId) => this.removeChat(chatId),
            now: this.now,
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

    private acquireWasmRuntime(
        signal?: AbortSignal,
    ): Promise<WasmRuntimeLease> {
        if (!this.wasmRuntimes) {
            throw new Error(
                'runtime.kind "transport" does not use a WASM runtime pool',
            );
        }
        return this.wasmRuntimes.acquire({ signal });
    }

    private removeChat(id: string): void {
        this.chats.delete(id);
        this.sessions.removeChat(id);
    }
}
