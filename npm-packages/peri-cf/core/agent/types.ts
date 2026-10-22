import type { PeriWasmHostLease, WasmAcpTransport } from "@peri-code/sdk/wasm";
import type { Chat, ChatSummary } from "../shared";

/** 服务端存储适配器：只读会话清单与身份，不创建会话。 */
export interface ChatSessionStore {
  /** 列表事实由存储提供；运行态字段由服务层合并，存储不伪造。 */
  list(): Promise<ChatSummary[]>;
  get(id: string): Promise<Chat | null>;
}

export type { Chat, ChatSummary } from "../shared";

/** 前端仓储端口：仍负责创建会话（服务端创建由 `ChatService` 编排）。 */
export interface ChatRepository {
  list(): Promise<ChatSummary[]>;
  get(id: string): Promise<Chat | null>;
  create(title: string): Promise<Chat>;
}

/** 宿主的一次启动；失败以 SDK 的宿主启动错误携带清理证据。 */
export type StartHost = (options: { signal: AbortSignal }) => Promise<WasmAcpTransport>;

/** 获取一个已启动宿主的所有权租约；启动/关闭证据由租约负责。 */
export type AcquireHost = (options: { signal: AbortSignal }) => Promise<PeriWasmHostLease>;

/** 保活入口：不阻塞调用方，默认 fire-and-forget。 */
export type Defer = (promise: Promise<unknown>) => void;

/** ACP `session/new|load` 的 MCP server 声明。 */
export type McpSetupServer =
  | { type: "http"; name: string; url: string; headers: { name: string; value: string }[] }
  | { type: "acp"; name: string; serverId: string };

export type McpPurpose = "create" | "load" | "prompt";

/** 一次宿主实例的 MCP 附属能力；服务端只提供远程 HTTP 声明。 */
export interface McpAttachment {
  servers: McpSetupServer[];
  /** 反向 `mcp/message` 请求入口。 */
  handleMessage(params: unknown): unknown;
  /** prompt 前等待工具目录结算；`settle` 为 true 时额外留出宿主提交窗口。 */
  ready?(options: { settle: boolean }): Promise<void>;
  /** `session/new` 之后冻结本会话的 MCP 快照（在 rename 前调用）。 */
  freeze?(chatId: string): void;
  close(reason?: string): Promise<void>;
}

/** 平台环境端口：会话 cwd、系统指令、MCP 与宿主初始化均由调用方注入。 */
export interface AgentEnvironment {
  /** ACP 会话 cwd。 */
  readonly cwd: string;
  /** 新会话冻结的系统指令。 */
  readonly instructions: string;
  /** ACP `initialize`（客户端身份与 capabilities）。 */
  initialize(transport: WasmAcpTransport): Promise<void>;
  /** 为一次宿主实例装配 MCP；`chatId` 在 load/prompt 时给出。 */
  openMcp(purpose: McpPurpose, chatId?: string): McpAttachment;
  /** 诊断出口；默认实现写 console。 */
  logError(label: string, error: unknown, context?: Record<string, unknown>): void;
}
