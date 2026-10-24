import { BareHarnessConfig, TursoStorage } from "@peri-code/sdk/portable";
import {
  DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS, PeriWasmHostManager, createSchedulerPort, startPeriWasmHost,
} from "@peri-code/sdk/wasm";
import {
  initializeTransport, logError as logDiagnostic,
  type AgentEnvironment, type Chat, type ChatCommand, type ChatCommandPayload,
  type ChatRuntimeSubscription, type ChatSessionStore, type ChatSummary, type StartHost,
  type SyncState,
} from "../../core/agent";
import { SESSION_CWD, WORKSPACE_CWD, mcpSetupServers, type ServerConfig } from "../config";
import { ChatService } from "./service";
import { TursoChatSessionStore, emptyStoreTolerant } from "../store";
import { SYSTEM_INSTRUCTIONS } from "../system-instructions";

/** 宿主启动诊断前缀；上下文只带阶段与 kind，不包含凭据。 */
const HOST_DIAGNOSTIC_LABEL = "Peri server host diagnostic";
/** 进程退出时批量关闭失败：逐条记录，不阻断其他宿主。 */
const HOST_SHUTDOWN_FAILED_LABEL = "Peri host shutdown failed";

/** 日志上下文里的生命周期阶段。 */
const HOST_START_PHASE = "host-start";
const HOST_SHUTDOWN_PHASE = "shutdown";

/** 远程 MCP 不发起 ACP 反向请求；收到即说明客户端能力声明与宿主不匹配。 */
const REMOTE_MCP_REVERSE_REQUEST_ERROR = "Remote MCP servers do not issue ACP reverse requests";
/** 只把遥测变量交给 WASM 实例，避免服务端凭据扩散到模块环境。 */
const WASM_ENV_PREFIX = "LANGFUSE_";
/** 宿主配置中的模型别名与 provider 标识；`providers` 与 `profiles` 必须保持一致。 */
const DEFAULT_MODEL_ALIAS = "sonnet";
const HARNESS_PROVIDER_ID = "model";

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

/** 与宿主 `configJson` 的形状一致；模型与存储直连上游，凭据只存在于服务端进程。 */
function harnessConfigJson(config: ServerConfig): string {
  return JSON.stringify({
    cwd: WORKSPACE_CWD,
    machineId: config.machineId,
    storage: { url: config.storage.url, authToken: config.storage.authToken },
    settings: {
      config: {
        active_alias: DEFAULT_MODEL_ALIAS,
        providers: [{
          id: HARNESS_PROVIDER_ID, type: config.model.provider, apiKey: config.model.apiKey,
          baseUrl: config.model.baseUrl,
        }],
        profiles: {
          [DEFAULT_MODEL_ALIAS]: { provider: HARNESS_PROVIDER_ID, model: config.model.id },
        },
        meta_harness: SERVER_HARNESS_CONFIG,
      },
    },
  });
}

/** 只把遥测相关变量交给 WASM 实例，避免把服务端凭据扩散到模块环境。 */
function wasmEnvironment(
  env: Readonly<Record<string, string | undefined>> = process.env,
): Record<string, string> {
  return Object.fromEntries(
    Object.entries(env).filter((entry): entry is [string, string] =>
      entry[0].startsWith(WASM_ENV_PREFIX) && entry[1] !== undefined),
  );
}

/** 生产宿主启动器：只在 `ChatManager` 装配时创建。 */
function createHostStarter(config: ServerConfig): StartHost {
  return ({ signal }) => startPeriWasmHost({
    configJson: harnessConfigJson(config),
    moduleUrl: config.wasmModuleUrl,
    env: wasmEnvironment(),
    signal,
    cleanupTimeoutMs: DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS,
    ports: { scheduler: createSchedulerPort() },
    onDiagnostic: (kind, error) => logDiagnostic(HOST_DIAGNOSTIC_LABEL, error, {
      phase: HOST_START_PHASE, kind,
    }),
  });
}

/**
 * 默认服务端环境：系统指令、宿主初始化与远程 MCP 声明；不启动本地 ACP/stdio MCP。
 * 每次调用返回独立对象；`chatId` 给出时只进入诊断上下文。
 */
function createDefaultEnvironment(
  config: ServerConfig, chatId?: string,
): AgentEnvironment & { close(): Promise<void> } {
  const servers = mcpSetupServers(config);
  return {
    cwd: SESSION_CWD,
    instructions: SYSTEM_INSTRUCTIONS,
    initialize: (transport) => initializeTransport(transport),
    openMcp: () => ({
      // 复制数组：会话内不得通过共享引用改动服务端配置。
      servers: servers.map((server) => ({
        ...server, headers: server.headers.map((header) => ({ ...header })),
      })),
      handleMessage(): never {
        throw new Error(REMOTE_MCP_REVERSE_REQUEST_ERROR);
      },
      async close(): Promise<void> {
        // 远程 HTTP 声明由宿主生命周期管理，没有本地 bridge 需要释放。
      },
    }),
    logError: (label, error, context) => logDiagnostic(
      label, error, chatId === undefined ? context : { chatId, ...context }),
    async close(): Promise<void> {
      // 远程 HTTP 声明由宿主生命周期管理，没有本地 bridge 需要释放。
    },
  };
}

/** 会话管理端口：API 的唯一依赖，也是 `ChatManager` 的对外契约。 */
export interface ChatManagerPort {
  /** 会话清单：存储事实 + 当前驻留实例的运行态。 */
  list(): Promise<ChatSummary[]>;
  /** 新建会话：一次性宿主完成 ACP 建会话，再从 Store 读取身份。 */
  create(title: string): Promise<Chat>;
  /** 读取会话最新投影；首次访问会触发冷复原。 */
  read(chatId: string): Promise<SyncState>;
  /** 执行命令；失败以 `ChatCommandError` 携带稳定码与处置。 */
  run(chatId: string, command: ChatCommand): Promise<ChatCommandPayload | undefined>;
  /** 订阅投影；先返回初始快照，再推送后续变化。 */
  watch(chatId: string, onState: (state: SyncState) => void): Promise<ChatRuntimeSubscription>;
  /** 释放驻留实例与所有存活宿主；幂等且可并发调用。 */
  shutdown(): Promise<void>;
}

/** `ChatManager` 的可替换依赖；生产默认全部由配置装配。 */
export interface ChatManagerOptions {
  /** 只读会话存储端口；生产由本类装配 Turso 适配器。 */
  readonly store?: ChatSessionStore;
  /** 测试可注入替身；生产启动 WASM 宿主。 */
  readonly startHost?: StartHost;
  /** 每个实例一个独立环境；创建会话不传 `chatId`。 */
  readonly createEnvironment?: (chatId?: string) => AgentEnvironment;
  /** 宿主生命周期诊断出口；默认写 console。 */
  readonly logError?: AgentEnvironment["logError"];
}

/**
 * 后端唯一入口与组合根：装配 Store、宿主租约管理器与环境工厂，向下传给 `ChatService`。
 * 自身不持有会话实例；宿主租约由 `PeriWasmHostManager` 统一登记与关闭。
 */
export class ChatManager implements ChatManagerPort {
  /** 会话服务：持有实例映射与驻留 LRU。 */
  private readonly service: ChatService;
  /** 宿主租约登记表：统一启动、跟踪与关闭证据。 */
  private readonly hosts: PeriWasmHostManager;
  /** 宿主生命周期诊断出口；默认写 console。 */
  private readonly logError: AgentEnvironment["logError"];
  /** 并发 `shutdown()` 共享同一轮关闭；结束后清空，允许下一轮兜底。 */
  private shutdownPromise: Promise<void> | undefined;

  /** 装配 Store、宿主租约管理器与环境工厂，并把同一组依赖向下注入 `ChatService`。 */
  constructor(config: ServerConfig, options: ChatManagerOptions = {}) {
    const store = options.store ?? new TursoChatSessionStore(emptyStoreTolerant(
      new TursoStorage({ url: config.storage.url, authToken: config.storage.authToken }),
    ));
    this.logError = options.logError ?? logDiagnostic;
    this.hosts = new PeriWasmHostManager(options.startHost ?? createHostStarter(config), {
      closeTimeoutMs: DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS,
    });
    this.service = new ChatService({
      store,
      acquireHost: (startOptions) => this.hosts.acquire(startOptions),
      createEnvironment: options.createEnvironment
        ?? ((chatId) => createDefaultEnvironment(config, chatId)),
    });
  }

  /** 会话清单：存储事实 + 驻留实例运行态。 */
  list(): Promise<ChatSummary[]> { return this.service.list(); }
  /** 新建会话；创建走 ACP，不在 Store 侧建行。 */
  create(title: string): Promise<Chat> { return this.service.create(title); }
  /** 读取最新投影；首次访问触发冷复原。 */
  read(chatId: string): Promise<SyncState> { return this.service.read(chatId); }
  /** 执行命令；返回载荷或 `undefined`。 */
  run(chatId: string, command: ChatCommand): Promise<ChatCommandPayload | undefined> {
    return this.service.run(chatId, command);
  }
  /** 订阅投影；订阅建立后由调用方负责关闭。 */
  watch(chatId: string, onState: (state: SyncState) => void): Promise<ChatRuntimeSubscription> {
    return this.service.watch(chatId, onState);
  }

  /**
   * 先释放驻留实例（各自关闭宿主），再兜底关闭未被接管的宿主（创建或启动中的实例）。
   * 幂等且可并发调用；单个关闭失败只记录诊断，不向调用方抛出。
   */
  shutdown(): Promise<void> {
    this.shutdownPromise ??= (async () => {
      await this.service.shutdown();
      const reports = await this.hosts.closeAll();
      for (const report of reports) {
        if (!report.confirmed)
          this.logError(HOST_SHUTDOWN_FAILED_LABEL, report.error, {
            phase: HOST_SHUTDOWN_PHASE, generationId: report.generationId,
          });
      }
    })().finally(() => { this.shutdownPromise = undefined; });
    return this.shutdownPromise;
  }
}
