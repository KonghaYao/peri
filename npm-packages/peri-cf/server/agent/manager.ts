import {
  AtomicSessionClaims,
  BareHarnessConfig,
  ChatCloseIncompleteError,
  ChatManager,
  MemoryKV,
  type ChatDatabase,
  type ChatDefaults,
  type ChatHost,
  type ChatLifecycleOptions,
  type ChatService,
  type ChatSessionDirectoryEntry,
  type SessionClaims,
} from "@peri-code/sdk";
import {
  createSchedulerPort,
  DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS,
} from "@peri-code/sdk/wasm";
import {
  SESSION_CWD,
  WORKSPACE_CWD,
  mcpServerDefaults,
  type ServerConfig,
} from "../config";
import { createChatDatabase } from "../store";
import { SYSTEM_INSTRUCTIONS } from "../system-instructions";
import { ChatViewRegistry } from "./views";

/**
 * 服务端组合根：进程级 SDK `ChatManager` 单例。
 *
 * Manager 只装配平台依赖（WASM 宿主、Store、Session claim、默认配置）；
 * 句柄、绑定、动作与事实都由 SDK 负责。`createChat` 同步登记，不启动 WASM。
 */

type SdkChatManagerOptions = ConstructorParameters<typeof ChatManager>[0];
/** SDK 内部工厂入口仅用于测试注入；生产由 Manager 默认装配。 */
export type ChatInstanceFactory = NonNullable<
  SdkChatManagerOptions["instanceFactory"]
>;

const DEFAULT_MODEL_ALIAS = "sonnet";
const HARNESS_PROVIDER_ID = "model";
const HOST_DIAGNOSTIC_LABEL = "Peri server host diagnostic";
const HOST_START_PHASE = "host-start";
const WASM_ENV_PREFIX = "LANGFUSE_";

/** 生产关闭预算与 WASM 宿主的清理预算对齐，避免把正常清理误报为 Incomplete。 */
const DEFAULT_LIFECYCLE: ChatLifecycleOptions = {
  activationCloseTimeoutMs: DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS,
  shutdownChatTimeoutMs: DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS + 10_000,
};

/**
 * 服务端宿主的工具面：对话、Agent、工具搜索与 goal 直接可见；文件/终端/Web 等
 * 进程内 builtin 在 WASM 部署不可用（`IN_PROCESS_BUILTINS=false`），只能由远程 MCP server 提供。
 */
const SERVER_HARNESS_CONFIG = Object.freeze({
  ...BareHarnessConfig,
  TodoMiddleware: true,
  HumanInTheLoopMiddleware: true,
  SubAgentMiddleware: true,
  BuiltInSubagents: true,
  McpMiddleware: true,
  ToolSearch: true,
  GoalMiddleware: true,
});

/** 只把遥测变量交给 WASM 实例，避免服务端凭据扩散到模块环境。 */
function wasmEnvironment(
  env: Readonly<Record<string, string | undefined>> = process.env,
): Record<string, string> {
  return Object.fromEntries(
    Object.entries(env).filter((entry): entry is [string, string] =>
      entry[0].startsWith(WASM_ENV_PREFIX) && entry[1] !== undefined),
  );
}

/** 与宿主 `configJson` 的形状一致；模型与存储直连上游，凭据只存在于服务端进程。 */
function harnessConfig(config: ServerConfig): Record<string, unknown> {
  return {
    cwd: WORKSPACE_CWD,
    machineId: config.machineId,
    storage: {
      url: config.storage.url,
      authToken: config.storage.authToken,
    },
    settings: {
      config: {
        active_alias: DEFAULT_MODEL_ALIAS,
        providers: [{
          id: HARNESS_PROVIDER_ID,
          type: config.model.provider,
          apiKey: config.model.apiKey,
          baseUrl: config.model.baseUrl,
        }],
        profiles: {
          [DEFAULT_MODEL_ALIAS]: {
            provider: HARNESS_PROVIDER_ID,
            model: config.model.id,
          },
        },
        meta_harness: SERVER_HARNESS_CONFIG,
      },
    },
  };
}

function defaultHost(
  config: ServerConfig,
  logError: NonNullable<AppChatManagerOptions["logError"]>,
): ChatHost {
  return {
    kind: "wasm",
    moduleUrl: config.wasmModuleUrl,
    env: wasmEnvironment(),
    ports: { scheduler: createSchedulerPort() },
    cleanupTimeoutMs: DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS,
    onDiagnostic: (kind, error) => logError(HOST_DIAGNOSTIC_LABEL, error, {
      phase: HOST_START_PHASE,
      kind,
    }),
  };
}

function defaultChatDefaults(config: ServerConfig): ChatDefaults {
  return {
    path: SESSION_CWD,
    instructions: SYSTEM_INSTRUCTIONS,
    mcpServers: mcpServerDefaults(config),
  };
}

export interface AppChatManagerOptions {
  readonly database?: ChatDatabase;
  readonly sessionClaims?: SessionClaims;
  readonly host?: ChatHost;
  readonly lifecycle?: ChatLifecycleOptions;
  readonly now?: () => string;
  readonly views?: ChatViewRegistry;
  /** 测试注入：替换 SDK 的 PeriInstance 工厂。 */
  readonly instanceFactory?: ChatInstanceFactory;
  readonly logError?: (label: string, error: unknown, context?: Record<string, unknown>) => void;
}

/**
 * CF 的进程级 Manager：SDK `ChatManager` + 视图注册表。
 *
 * `shutdown` 先释放 View（断开同步连接），再让 SDK 聚合关闭句柄与宿主；
 * 两步失败都上报，失败时保留 Manager 供再次重试。
 */
export class AppChatManager {
  readonly chats: ChatManager;
  readonly views: ChatViewRegistry;

  constructor(config: ServerConfig, options: AppChatManagerOptions = {}) {
    const lifecycle = { ...DEFAULT_LIFECYCLE, ...options.lifecycle };
    const logError = options.logError
      ?? ((label: string, error: unknown, context?: Record<string, unknown>) =>
        console.error(`[peri-cf] ${label}`, context ?? {}, error));
    this.views = options.views ?? new ChatViewRegistry();
    const managerOptions: SdkChatManagerOptions = {
      agentConfig: harnessConfig(config),
      database: options.database ?? createChatDatabase(config),
      sessionClaims: options.sessionClaims
        ?? new AtomicSessionClaims(new MemoryKV()),
      host: options.host ?? defaultHost(config, logError),
      defaults: defaultChatDefaults(config),
      lifecycle,
      now: options.now,
      ...(options.instanceFactory
        ? { instanceFactory: options.instanceFactory }
        : {}),
    };
    this.chats = new ChatManager(managerOptions);
  }

  createChat(options?: { path?: string }): ChatService {
    return this.chats.createChat(options);
  }

  getChat(id: string): ChatService | undefined {
    return this.chats.getChat(id);
  }

  findChatBySession(sessionId: string): ChatService | undefined {
    return this.chats.findChatBySession(sessionId);
  }

  /** 目录查询只读 Store，不启动历史运行实例。 */
  listSessions(options?: { path?: string }): Promise<ChatSessionDirectoryEntry[]> {
    return this.chats.listSessions(options);
  }

  /** 成功关闭后同时释放该 Chat 的视图；任一步失败都保留句柄/占用或显式上报。 */
  async closeChat(id: string): Promise<void> {
    await this.chats.closeChat(id);
    try {
      await this.views.dispose(id);
    } catch (error) {
      throw new ChatCloseIncompleteError(
        [error],
        "Chat is closed but view disposal is incomplete",
      );
    }
  }

  viewFor(chat: ChatService) {
    return this.views.attach(chat);
  }

  async shutdown(): Promise<void> {
    const failures: unknown[] = [];
    try {
      await this.views.disposeAll();
    } catch (error) {
      failures.push(error);
    }
    try {
      await this.chats.shutdown();
    } catch (error) {
      failures.push(error);
    }
    if (failures.length > 0) {
      throw new AggregateError(
        failures,
        "Peri CF chat manager shutdown is incomplete",
      );
    }
  }
}

export function createAppChatManager(
  config: ServerConfig,
  options: AppChatManagerOptions = {},
): AppChatManager {
  return new AppChatManager(config, options);
}

let processManager: AppChatManager | undefined;

/**
 * 进程级单例：同一进程只创建一个 Manager。
 *
 * 首次调用才构造依赖；WASM 宿主仍由首次激活懒启动。测试用
 * `resetChatManagerForTest` 隔离，生产入口只在 `server/index.ts` 调用。
 */
export function getChatManager(
  config: ServerConfig,
  options: AppChatManagerOptions = {},
): AppChatManager {
  processManager ??= createAppChatManager(config, options);
  return processManager;
}

/** 关闭成功后清空单例；失败保留，允许再次 shutdown。 */
export async function shutdownChatManager(): Promise<void> {
  const manager = processManager;
  if (!manager) return;
  await manager.shutdown();
  processManager = undefined;
}

export function resetChatManagerForTest(): void {
  processManager = undefined;
}
