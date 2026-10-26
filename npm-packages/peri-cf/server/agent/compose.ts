import {
  AtomicSessionOwnership,
  BareHarnessConfig,
  ChatManager,
  MemoryKV,
  type ChatDatabase,
  type ChatDefaults,
  type ChatRuntime,
  type ChatLifecycleOptions,
  type SessionOwnership,
} from "@peri-code/sdk";
import {
  createSchedulerPort,
  DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS,
} from "@peri-code/sdk/wasm";
import {
  SESSION_CWD,
  WORKSPACE_CWD,
  mcpServerDefaults,
  type ServerConfig,
} from "../config";
import { createChatDatabase } from "../store";
import { SYSTEM_INSTRUCTIONS } from "../system-instructions";

/**
 * 平台装配：把 CF 的配置、Store、WASM 运行后端与默认工具面装配成 SDK `ChatManager`。
 *
 * 这是 server 内唯一构造具体 `ChatManager` 的位置。编排层只接收注入的
 * `ChatManagerContract`，本文件承担全部平台读取（env、WASM、Store、默认配置）。
 */

type ChatManagerConstructorOptions = ConstructorParameters<typeof ChatManager>[0];

/** SDK 内部工厂入口仅用于测试注入；生产由 ChatManager 默认装配。 */
export type ChatInstanceFactory = NonNullable<
  ChatManagerConstructorOptions["instanceFactory"]
>;

export type LogError = (
  label: string,
  error: unknown,
  context?: Record<string, unknown>,
) => void;

/** 平台 seam：省略时使用 CF 的生产默认值。 */
export interface SdkChatManagerOptions {
  readonly database?: ChatDatabase;
  readonly sessionOwnership?: SessionOwnership;
  readonly runtime?: ChatRuntime;
  readonly lifecycle?: ChatLifecycleOptions;
  readonly now?: () => string;
  readonly instanceFactory?: ChatInstanceFactory;
  readonly logError?: LogError;
}

const DEFAULT_MODEL_ALIAS = "sonnet";
const HARNESS_PROVIDER_ID = "model";
const RUNTIME_DIAGNOSTIC_LABEL = "Peri server runtime diagnostic";
const RUNTIME_START_PHASE = "runtime-start";
const WASM_ENV_PREFIX = "LANGFUSE_";

/** 生产关闭预算与 WASM 运行后端的清理预算对齐，避免把正常清理误报为 Incomplete。 */
const DEFAULT_LIFECYCLE: ChatLifecycleOptions = {
  activationCloseTimeoutMs: DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS,
  shutdownChatTimeoutMs: DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS + 10_000,
};

/**
 * 服务端运行后端的工具面：对话、Agent、工具搜索与 goal 直接可见；文件/终端/Web 等
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

function defaultRuntime(config: ServerConfig, logError: LogError): ChatRuntime {
  return {
    kind: "wasm",
    moduleUrl: config.wasmModuleUrl,
    env: wasmEnvironment(),
    ports: { scheduler: createSchedulerPort() },
    cleanupTimeoutMs: DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS,
    onDiagnostic: (kind, error) => logError(RUNTIME_DIAGNOSTIC_LABEL, error, {
      phase: RUNTIME_START_PHASE,
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

function consoleLogError(label: string, error: unknown, context?: Record<string, unknown>): void {
  console.error(`[peri-cf] ${label}`, context ?? {}, error);
}

/** 装配 SDK 默认实现；测试与其它宿主通过 `SdkChatManagerOptions` 替换平台 seam。 */
export function createSdkChatManager(
  config: ServerConfig,
  options: SdkChatManagerOptions = {},
): ChatManager {
  const logError = options.logError ?? consoleLogError;
  return new ChatManager({
    agentConfig: harnessConfig(config),
    database: options.database ?? createChatDatabase(config),
    sessionOwnership: options.sessionOwnership
      ?? new AtomicSessionOwnership(new MemoryKV()),
    runtime: options.runtime ?? defaultRuntime(config, logError),
    defaults: defaultChatDefaults(config),
    lifecycle: { ...DEFAULT_LIFECYCLE, ...options.lifecycle },
    now: options.now,
    instanceFactory: options.instanceFactory,
  });
}
