import { createStorage } from "unstorage";
import {
  BareHarnessConfig,
  TursoStorage,
  type ChatAgentConfig,
  type ChatManagerOptions,
  type ChatSessionOptions,
  type WorkspaceRef,
} from "@peri-code/sdk";
import { logError } from "./logging";
import {
  SESSION_CWD,
  WORKSPACE_CWD,
  MODEL_TIERS,
  mcpServerDefaults,
  type ServerConfig,
} from "./config";
import { SYSTEM_INSTRUCTIONS } from "./system-instructions";

/**
 * 平台装配：把 CF 的配置、Store、WASM 运行后端与默认工具面装配成 SDK `ChatManager`
 * 的构造选项，并提供每个 Chat 共用的 `defaultAgentConfig`。
 *
 * `chatManagerOptions` 只做纯装配，测试可直接断言；生产路径不提供替换 Store、KV、
 * 运行后端或日志的注入 seam。具体 `ChatManager` 由入口用这些选项构造。
 */

const DEFAULT_WORKSPACE_ID = "default";
/** 服务端唯一活动档位；`defaultAgentConfig` 与只读监控共用。 */
export const DEFAULT_MODEL_ALIAS = "sonnet";
const HARNESS_PROVIDER_ID = "model";
const RUNTIME_DIAGNOSTIC_LABEL = "[peri-cf] Peri server runtime diagnostic";
const RUNTIME_START_PHASE = "runtime-start";

/**
 * 转发给 WASM 的遥测环境变量允许列表。
 *
 * 单一权威是 `peri-config/src/observability.rs::ENVIRONMENT_KEYS`（spool 相关键定义在
 * `observability_spool.rs`）；Rust 新增或删除环境键时必须同步本列表。只列入确有 Rust
 * 读取点的键，`LANGFUSE_TRACING` 等同前缀但无人读取的键不转发。
 */
const LANGFUSE_ENV_KEYS = [
  "LANGFUSE_PUBLIC_KEY",
  "LANGFUSE_SECRET_KEY",
  "LANGFUSE_BASE_URL",
  "LANGFUSE_TRACE_SAMPLING",
  "LANGFUSE_ERROR_SPAN_ALWAYS",
  "LANGFUSE_BATCH_MAX_EVENTS",
  "LANGFUSE_BATCH_QUEUE_CAPACITY",
  "LANGFUSE_BATCH_MAX_IN_FLIGHT",
  "LANGFUSE_BATCH_MAX_EVENT_BYTES",
  "LANGFUSE_BATCH_MAX_BYTES",
  "LANGFUSE_BATCH_MAX_QUEUE_BYTES",
  "LANGFUSE_BATCH_FLUSH_INTERVAL",
  "LANGFUSE_USER_ID",
  "LANGFUSE_SPOOL_ENABLED",
  "LANGFUSE_COMPRESSION",
  "LANGFUSE_INPUT_SPOOL_THRESHOLD_BYTES",
  "LANGFUSE_PAYLOAD_MEMORY_THRESHOLD_BYTES",
  "LANGFUSE_MEMORY_BUDGET_BYTES",
  "LANGFUSE_PREPARE_MAX_IN_FLIGHT",
  "LANGFUSE_STREAM_CHUNK_BYTES",
  "LANGFUSE_SPOOL_QUOTA_BYTES",
  "LANGFUSE_SPOOL_TTL_SECS",
  "LANGFUSE_MAX_WIRE_BYTES",
  "LANGFUSE_SHUTDOWN_GRACE_SECS",
] as const;

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

/** 只把有 Rust 读取点的遥测变量交给 WASM 实例，避免服务端凭据扩散到模块环境。 */
function wasmEnvironment(
  env: Readonly<Record<string, string | undefined>> = process.env,
): Record<string, string> {
  const selected: Record<string, string> = {};
  for (const key of LANGFUSE_ENV_KEYS) {
    const value = env[key];
    if (value !== undefined) selected[key] = value;
  }
  return selected;
}

/**
 * peri-cf 只有固定工作区：宿主目录与运行时根都是 `/workspace`。
 *
 * `id` 固定，保证同一进程内所有 Chat 的 Session 归属一致；`config` 目前不参与
 * 派生，保留参数与 `defaultAgentConfig(config)` 对称，多工作区时从这里扩展。
 */
export function defaultWorkspace(_config: ServerConfig): WorkspaceRef {
  return {
    id: DEFAULT_WORKSPACE_ID,
    hostPath: WORKSPACE_CWD,
    runtimePath: WORKSPACE_CWD,
  };
}

/**
 * 单个 Chat 的运行配置：模型、四档 profiles 与工具面都来自服务端配置。
 *
 * `cwd` 不在此声明：SDK 从 `workspace.runtimePath` 派生并注入，显式声明反而
 * 可能与工作区不一致。`storage`/`machineId` 属于 Manager 部署边界（见
 * `chatManagerOptions.store`），不能出现在 per-chat 配置里；
 * `ChatRegistry` 每次创建句柄时传入本对象。
 */
export function defaultAgentConfig(config: ServerConfig): ChatAgentConfig {
  return {
    // 与宿主 `configJson` 的形状一致；模型直连上游，凭据只存在于服务端进程。
    settings: {
      config: {
        active_alias: DEFAULT_MODEL_ALIAS,
        providers: [{
          id: HARNESS_PROVIDER_ID,
          type: config.model.provider,
          apiKey: config.model.apiKey,
          baseUrl: config.model.baseUrl,
          // 四档模型 ID 只在这里声明一次；profiles 只绑定 provider，避免两处漂移。
          models: config.model.tiers,
        }],
        profiles: Object.fromEntries(
          MODEL_TIERS.map((tier) => [tier, { provider: HARNESS_PROVIDER_ID }]),
        ),
        meta_harness: SERVER_HARNESS_CONFIG,
      },
    },
  };
}

/** 进程共享的只读会话目录；注册表与 Manager 必须持有同一实例。 */
export function createChatDatabase(config: ServerConfig): TursoStorage {
  return new TursoStorage({
    ...config.storage,
    legacyCwd: WORKSPACE_CWD,
    currentCwd: SESSION_CWD,
  });
}

/** 每个 Chat 共享的 Session 建立配置；sandbox MCP 在注册表按 Chat 合并。 */
export function defaultSessionOptions(config: ServerConfig): ChatSessionOptions {
  return {
    // cwd 由 `createChat({ workspace, sessionPath })` 派生；这里只放共享配置。
    instructions: SYSTEM_INSTRUCTIONS,
    mcpServers: mcpServerDefaults(config),
  };
}

/** 纯装配：返回 SDK `ChatManager` 的完整构造选项，不启动运行后端。 */
export function chatManagerOptions(
  config: ServerConfig,
  database = createChatDatabase(config),
): ChatManagerOptions {
  return {
    // Manager 是部署边界：Store 与机器身份整个进程共享，per-chat 不可声明。
    store: {
      storage: database.storageConfig,
      machineId: config.machineId,
    },
    database,
    // 默认进程内 memory storage；跨实例接管需要路由层与共享 storage，本轮不接入。
    kv: createStorage(),
    chatConfig: {
      runtime: {
        kind: "wasm",
        moduleUrl: config.wasmModuleUrl,
        env: wasmEnvironment(),
        onDiagnostic: (kind, error) => logError(RUNTIME_DIAGNOSTIC_LABEL, error, {
          phase: RUNTIME_START_PHASE,
          kind,
        }),
      },
      session: defaultSessionOptions(config),
    },
  };
}
