import { createStorage } from "unstorage";
import {
  BareHarnessConfig,
  TursoStorage,
  type ChatManagerOptions,
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
 * 的构造选项。
 *
 * `chatManagerOptions` 只做纯装配，测试可直接断言；生产路径不提供替换 Store、KV、
 * 运行后端或日志的注入 seam。具体 `ChatManager` 由入口用这些选项构造。
 */

const DEFAULT_MODEL_ALIAS = "sonnet";
const HARNESS_PROVIDER_ID = "model";
const RUNTIME_DIAGNOSTIC_LABEL = "[peri-cf] Peri server runtime diagnostic";
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

/** 纯装配：返回 SDK `ChatManager` 的完整构造选项，不启动运行后端。 */
export function chatManagerOptions(config: ServerConfig): ChatManagerOptions {
  const defaultDatabase = new TursoStorage({
    ...config.storage,
    legacyCwd: WORKSPACE_CWD,
    currentCwd: SESSION_CWD,
  });
  return {
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
    database: defaultDatabase,
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
      sessionInit: {
        path: SESSION_CWD,
        instructions: SYSTEM_INSTRUCTIONS,
        mcpServers: mcpServerDefaults(config),
      },
    },
  };
}
