import { WasmRuntimePool, type WasmRuntimeStarter } from "../wasm/runtime-pool";
import { startWasmRuntime } from "../wasm/runtime";
import type { ChatDatabase } from "../contracts/chat";
import type { WorkspaceRef } from "../workspace/types";
import { snapshotChatStore, resolveChatAgentConfig } from "./agent-config";
import { createChatInstance } from "./instance-factory";
import { mergeChatLifecycle, resolveChatLifecycle } from "./lifecycle";
import { KvSessionOwnership } from "./ownership";
import { copyChatSessionInit } from "./session-init";
import { createChatSessionIndex } from "./session-index";
import { ChatService } from "./service";
import type { ChatServiceOptions } from "./service-context";
import type {
    ChatAgentConfig,
    ChatConfig,
    ChatLifecycleOptions,
    ChatSessionOptions,
    ChatRuntime,
    KvStorage,
} from "./types";
import { resolveChatWorkspace } from "./workspace-config";

/**
 * 单个 Session 的 ChatService 装配输入。
 *
 * 每项依赖都按「一次调用一个会话」声明：没有跨会话的目录、清单或共享运行池。
 * 调用方持有 `database`/`ownership`，并负责在进程退出时关闭它们。
 */
export interface CreateChatSessionInput {
    /** 该 Session 固定不变的目标；装配时不启动 ACP、不取得 Session ownership。 */
    readonly target: {
        /** 目标 Session ID；同一 Session 的装配结果都以它为身份。 */
        readonly sessionId: string;
        readonly workspace: WorkspaceRef;
        readonly agent: ChatAgentConfig;
        /** 写入 agentConfig 的 Store 描述；工厂只做快照，不解析 URL。 */
        readonly storage: { readonly url: string; readonly authToken?: string };
        /** 相对 `workspace.runtimePath` 的会话 cwd；省略时为 workspace 根。 */
        readonly sessionPath?: string;
        readonly session?: ChatConfig["session"];
        readonly lifecycle?: ChatConfig["lifecycle"];
    };
    /** 只读会话目录；连接归调用方所有，工厂不会关闭它。 */
    readonly database: ChatDatabase;
    /** 共享 ownership 存储；工厂据此为本次调用建立独立的 owner。 */
    readonly ownership: Pick<KvStorage, "getItem" | "setItem" | "removeItem">;
    readonly machineId: string;
    readonly chatConfig: ChatConfig;
}

/** 测试接缝：替代默认 WASM 启动器；公开入口不导出该字段。 */
export interface CreateChatSessionInternalInput extends CreateChatSessionInput {
    readonly instanceFactory?: ChatServiceOptions["instanceFactory"];
    readonly wasmRuntimeStarter?: WasmRuntimeStarter;
}

/** 单次装配的封闭标记；`onClosed` 只改这里，不触碰任何跨会话条目。 */
interface SessionState {
    readonly sessionId: string;
    closed: boolean;
}

/** 句柄 → 会话状态；WeakMap 不会让句柄常驻，也不是跨会话清单。 */
const sessionStates = new WeakMap<ChatService, SessionState>();

/**
 * 包内只读访问器：取该句柄绑定的 Session 身份与封闭标记。
 *
 * 不导出到 `entries/chat.ts`；调用方用 `chat.close()` 的结果判断关闭，不需要轮询。
 */
export function readChatSessionState(
    chat: ChatService,
): { readonly sessionId: string; readonly closed: boolean } | undefined {
    const state = sessionStates.get(chat);
    return state === undefined
        ? undefined
        : { sessionId: state.sessionId, closed: state.closed };
}

/**
 * 为一个 Session 装配单个 `ChatService`（等价于 `ChatManager.createChat` 的单会话路径）。
 *
 * 装配顺序与 Manager 一致：workspace → agentConfig → sessionInit → ownership/索引 →
 * 实例工厂 → 每会话 WASM 池。任何一个校验或构造步骤失败都原样抛出，且不残留半开资源：
 * 运行后端只在首次激活时获取租约，构造期间不启动进程或 transport。
 */
export async function createChatSession(
    input: CreateChatSessionInput,
): Promise<ChatService> {
    return assembleChatSession(input, {});
}

/**
 * 包内装配入口，允许注入实例工厂与 WASM 启动器。
 *
 * 公开面只用 `createChatSession`；这里不导出到 `entries/chat.ts`。
 */
export async function createChatSessionInternal(
    input: CreateChatSessionInternalInput,
): Promise<ChatService> {
    return assembleChatSession(input, {
        ...(input.instanceFactory === undefined
            ? {}
            : { instanceFactory: input.instanceFactory }),
        ...(input.wasmRuntimeStarter === undefined
            ? {}
            : { wasmRuntimeStarter: input.wasmRuntimeStarter }),
    });
}

interface AssemblySeams {
    readonly instanceFactory?: ChatServiceOptions["instanceFactory"];
    readonly wasmRuntimeStarter?: WasmRuntimeStarter;
}

async function assembleChatSession(
    input: CreateChatSessionInput,
    seams: AssemblySeams,
): Promise<ChatService> {
    const target = requireTarget(input);
    const runtime = resolveRuntime(input.chatConfig);
    const store = snapshotChatStore({
        storage: snapshotStorage(target.storage),
        machineId: input.machineId,
    });

    // workspace 与 agent 都是单会话必填；cwd 由 workspace 派生并注入。
    const { workspace, runtimeRoot, sessionCwd } = resolveChatWorkspace(
        target.workspace,
        target.sessionPath,
    );
    const { agentConfig, agentConfigJson } = resolveChatAgentConfig(
        store,
        target.agent,
        runtimeRoot,
    );
    // 单会话的 session 覆盖 Manager 默认值；cwd 一律由 workspace 派生。
    const sessionInit = {
        ...copyChatSessionInit(target.session ?? input.chatConfig.session),
        path: sessionCwd,
    };
    const lifecycle = mergeChatLifecycle(
        resolveChatLifecycle(runtime.kind, input.chatConfig.lifecycle),
        runtime.kind,
        target.lifecycle,
    );

    const state: SessionState = { sessionId: target.sessionId, closed: false };
    const ownership = new KvSessionOwnership(input.ownership);
    const sessionIndex = createChatSessionIndex();
    const wasmRuntimes = createRuntimePool(runtime, seams.wasmRuntimeStarter);

    const chat = new ChatService({
        // 句柄身份沿用 Manager 的随机 id；Session 身份只由 ownership/索引表达。
        id: crypto.randomUUID(),
        workspace,
        sessionCwd,
        sessionInit,
        agentConfig,
        agentConfigJson,
        runtime,
        ...(runtime.kind === "wasm" && runtime.env !== undefined
            ? { runtimeEnv: { ...runtime.env } }
            : {}),
        ...(runtime.kind === "wasm" && runtime.onDiagnostic !== undefined
            ? { onRuntimeDiagnostic: runtime.onDiagnostic }
            : {}),
        database: input.database,
        ownership,
        index: sessionIndex,
        instanceFactory: seams.instanceFactory ?? createChatInstance,
        lifecycle,
        acquireWasmRuntime: (acquireOptions) => {
            // 关闭后不再启动运行后端：没有 Manager 承担关闭时的池级收敛。
            if (state.closed) {
                throw new Error(
                    "Chat session is closed; no new WASM runtime can be acquired",
                );
            }
            if (!wasmRuntimes) {
                throw new Error(
                    'runtime.kind "transport" does not use a WASM runtime pool',
                );
            }
            return wasmRuntimes.acquire(acquireOptions);
        },
        onClosed: () => {
            state.closed = true;
        },
    });
    sessionStates.set(chat, state);
    return chat;
}

function requireTarget(
    input: CreateChatSessionInput,
): CreateChatSessionInput["target"] {
    if (!input || typeof input !== "object") {
        throw new TypeError("createChatSession requires an options object");
    }
    const target = input.target;
    if (!target || typeof target !== "object") {
        throw new TypeError(
            "createChatSession requires target: { sessionId, workspace, agent, storage }",
        );
    }
    if (typeof target.sessionId !== "string" || target.sessionId.length === 0) {
        throw new TypeError(
            "createChatSession target.sessionId must be a non-empty string",
        );
    }
    return target;
}

function resolveRuntime(chatConfig: ChatConfig): ChatRuntime {
    if (
        !chatConfig ||
        typeof chatConfig !== "object" ||
        !chatConfig.runtime ||
        (chatConfig.runtime.kind !== "wasm" &&
            chatConfig.runtime.kind !== "transport")
    ) {
        throw new TypeError(
            'createChatSession requires chatConfig.runtime of { kind: "wasm" } or { kind: "transport" }',
        );
    }
    return chatConfig.runtime;
}

/**
 * 快照 Store 描述：只保留 string 字段，避免 `authToken: undefined` 破坏 JSON 校验。
 */
function snapshotStorage(
    storage: CreateChatSessionInput["target"]["storage"],
): Record<string, unknown> {
    if (!storage || typeof storage !== "object") {
        throw new TypeError(
            "createChatSession target.storage requires { url, authToken? }",
        );
    }
    if (typeof storage.url !== "string" || storage.url.length === 0) {
        throw new TypeError(
            "createChatSession target.storage.url must be a non-empty string",
        );
    }
    if (
        storage.authToken !== undefined &&
        typeof storage.authToken !== "string"
    ) {
        throw new TypeError(
            "createChatSession target.storage.authToken must be a string",
        );
    }
    return {
        url: storage.url,
        ...(storage.authToken === undefined
            ? {}
            : { authToken: storage.authToken }),
    };
}

/** WASM 池按会话建立；transport runtime 不持有池。 */
function createRuntimePool(
    runtime: ChatRuntime,
    starterOverride?: WasmRuntimeStarter,
): WasmRuntimePool | undefined {
    if (runtime.kind !== "wasm") return undefined;
    const starter: WasmRuntimeStarter =
        starterOverride ??
        (({ signal, configJson, env, onDiagnostic }) => {
            if (configJson === undefined) {
                throw new Error(
                    "WASM runtime requires a per-chat agentConfig snapshot",
                );
            }
            return startWasmRuntime({
                configJson,
                moduleUrl: runtime.moduleUrl,
                moduleFactory: runtime.moduleFactory,
                env: env ?? runtime.env,
                signal,
                onDiagnostic: onDiagnostic ?? runtime.onDiagnostic,
            });
        });
    return new WasmRuntimePool(starter);
}
