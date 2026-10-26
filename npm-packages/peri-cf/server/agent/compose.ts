import { createStorage } from "unstorage";
import {
  BareHarnessConfig,
  ChatManager,
  TursoStorage,
  type ChatConfig,
  type ChatDatabase,
  type KvStorage,
} from "@peri-code/sdk";
import {
  SESSION_CWD,
  WORKSPACE_CWD,
  MODEL_TIERS,
  mcpServerDefaults,
  type ServerConfig,
} from "../config";
import { SYSTEM_INSTRUCTIONS } from "../system-instructions";

/**
 * 平台装配：把 CF 的配置、Store、WASM 运行后端与默认工具面装配成 SDK `ChatManager`。
 *
 * 这是 server 内唯一构造具体 `ChatManager` 的位置。配置在这里按字段读取，
 * 不再向编排层传递整棵 `ServerConfig` 或派生的窄配置对象。
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
  readonly kv?: KvStorage;
  readonly chatConfig?: Partial<Pick<ChatConfig, "runtime" | "lifecycle">>;
  readonly instanceFactory?: ChatInstanceFactory;
  readonly logError?: LogError;
}

const DEFAULT_MODEL_ALIAS = "sonnet";
const HARNESS_PROVIDER_ID = "model";
const RUNTIME_DIAGNOSTIC_LABEL = "Peri server runtime diagnostic";
const RUNTIME_START_PHASE = "runtime-start";
const WASM_ENV_PREFIX = "LANGFUSE_";

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

function consoleLogError(label: string, error: unknown, context?: Record<string, unknown>): void {
  console.error(`[peri-cf] ${label}`, context ?? {}, error);
}

/** 装配 SDK 默认实现；测试与其它宿主通过 `SdkChatManagerOptions` 替换平台 seam。 */
export function createSdkChatManager(
  config: ServerConfig,
  options: SdkChatManagerOptions = {},
): ChatManager {
  const logError = options.logError ?? consoleLogError;
  const defaultDatabase = new TursoStorage({
    ...config.storage,
    legacyCwd: WORKSPACE_CWD,
    currentCwd: SESSION_CWD,
  });
  return new ChatManager({
    // 与宿主 `configJson` 的形状一致；模型与存储直连上游，凭据只存在于服务端进程。
    agentConfig: {
      cwd: WORKSPACE_CWD,
      machineId: config.machineId,
      storage: defaultDatabase.storageConfig,
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
    },
    database: options.database ?? defaultDatabase,
    // 默认进程内 memory storage；跨实例接管需要路由层与共享 storage，本轮不接入。
    kv: options.kv ?? createStorage(),
    chatConfig: {
      runtime: options.chatConfig?.runtime ?? {
        kind: "wasm",
        moduleUrl: config.wasmModuleUrl,
        env: wasmEnvironment(),
        onDiagnostic: (kind, error) => logError(RUNTIME_DIAGNOSTIC_LABEL, error, {
          phase: RUNTIME_START_PHASE,
          kind,
        }),
      },
      sessionInit: {
        path: SESSION_CWD,
        instructions: SYSTEM_INSTRUCTIONS,
        mcpServers: mcpServerDefaults(config),
      },
      lifecycle: options.chatConfig?.lifecycle,
    },
    instanceFactory: options.instanceFactory,
  });
}
