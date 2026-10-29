import type { ChatSessionDirectoryEntry } from "@peri-code/sdk";
import { z } from "zod";

/** 展示身份：`id` 与目录会话 ID 一致，标题/时间只来自目录或已绑定的副本。 */
export const chatSchema = z.object({
  id: z.string().min(1),
  title: z.string(),
  updatedAt: z.string(),
});

/**
 * 目录条目：字段与 SDK `ChatSessionDirectoryEntry` 一一对应，身份只有 `sessionId`。
 *
 * 运行态不在列表里伪造；当前选中会话的运行/交互事实只能来自 Yjs 副本。
 * 展示身份 `Chat.id` 由 `toChat` 从 `sessionId` 派生，不进入目录契约。
 */
const chatSummarySchema = z.object({
  sessionId: z.string().min(1),
  title: z.string().nullable(),
  cwd: z.string(),
  messageCount: z.number().int().nonnegative(),
  createdAt: z.string(),
  updatedAt: z.string(),
  /** 服务端按会话 cwd 推导；Store 目录本身不保存项目字段。 */
  projectId: z.string().min(1),
});

/** 项目是 sandbox 工作区的展示归属；默认项目聚合未归组会话。 */
export const projectSchema = z.object({
  id: z.string().min(1),
  name: z.string().min(1),
  createdAt: z.string(),
  isDefault: z.boolean(),
});

export const projectListResponseSchema = z.object({
  projects: z.array(projectSchema),
});

export const projectCreateResponseSchema = z.object({
  project: projectSchema,
});

export const chatDirectoryResponseSchema = z.object({
  chats: z.array(chatSummarySchema),
  available: z.boolean(),
  error: z.string().optional(),
});

/** `POST /api/chats` 只登记句柄；`sessionId:null` + `bindingRevision:0` 表示尚未绑定。 */
export const chatHandleSchema = z.object({
  id: z.string().min(1),
  sessionId: z.string().min(1).nullable(),
  bindingRevision: z.number().int().nonnegative(),
});

const sandboxStateSchema = z.enum([
  "idle", "starting", "ready", "stopping", "stopped", "failed", "closed", "unknown",
]);

/**
 * `GET /api/monitor` 的只读快照：LLM、MCP、WASM 实例与 Sandbox 四段。
 *
 * 地址在服务端去掉 userinfo、query 与 hash；MCP header 只保留名称，模型/Store
 * 凭据从不出网。WASM 段给出每个实例的线性内存容量（不是活跃堆）与业务计数，
 * 进程级 `arrayBufferBytes` 仍是进程合计。
 */
const wasmInstanceStateSchema = z.enum(["live", "closing", "closed", "close-unconfirmed"]);

const wasmInstanceSchema = z.object({
  chatId: z.string().min(1),
  sessionId: z.string().min(1).nullable(),
  generationId: z.string().min(1),
  state: wasmInstanceStateSchema,
  startedAt: z.string(),
  uptimeSeconds: z.number().nonnegative(),
  linearMemoryBytes: z.number().int().nonnegative().nullable(),
  running: z.boolean(),
  lastActivityAt: z.string().nullable(),
  queueDepth: z.number().int().nonnegative(),
  pendingInteractions: z.number().int().nonnegative(),
  turns: z.object({
    started: z.number().int().nonnegative(),
    completed: z.number().int().nonnegative(),
    cancelled: z.number().int().nonnegative(),
    error: z.number().int().nonnegative(),
    totalWallMs: z.number().nonnegative(),
    lastWallMs: z.number().nonnegative().nullable(),
    activeWallMs: z.number().nonnegative().nullable(),
  }),
  tokens: z.object({
    llmCalls: z.number().int().nonnegative(),
    inputTokens: z.number().int().nonnegative().nullable(),
    outputTokens: z.number().int().nonnegative().nullable(),
    auxiliaryCalls: z.number().int().nonnegative(),
    partial: z.boolean(),
  }).nullable(),
  context: z.object({
    used: z.number().nonnegative(),
    size: z.number().nonnegative(),
  }).nullable(),
  tools: z.object({
    started: z.number().int().nonnegative(),
    completed: z.number().int().nonnegative(),
    failed: z.number().int().nonnegative(),
    cancelled: z.number().int().nonnegative(),
  }).nullable(),
  lastStopReason: z.string().nullable(),
  statsError: z.literal(true).optional(),
});

export const monitorSnapshotSchema = z.object({
  ok: z.literal(true),
  generatedAt: z.string(),
  uptimeSeconds: z.number().nonnegative(),
  llm: z.object({
    provider: z.string().min(1),
    activeAlias: z.string().min(1),
    baseUrl: z.string(),
    tiers: z.record(z.string(), z.string()),
  }),
  mcp: z.object({
    remote: z.array(z.object({
      name: z.string().min(1),
      url: z.string(),
      headerNames: z.array(z.string()),
    })),
  }),
  wasm: z.object({
    moduleUrl: z.string(),
    process: z.object({
      rssBytes: z.number().int().nonnegative(),
      heapTotalBytes: z.number().int().nonnegative(),
      heapUsedBytes: z.number().int().nonnegative(),
      arrayBufferBytes: z.number().int().nonnegative(),
    }),
    chatHandles: z.number().int().nonnegative(),
    startingInstances: z.number().int().nonnegative(),
    unconfirmed: z.boolean(),
    instancesError: z.string().optional(),
    truncated: z.boolean(),
    linearMemoryTotalBytes: z.number().int().nonnegative().nullable(),
    linearMemoryKnownInstances: z.number().int().nonnegative(),
    linearMemoryUnknownInstances: z.number().int().nonnegative(),
    instances: z.array(wasmInstanceSchema),
  }),
  sandbox: z.object({
    enabled: z.boolean(),
    root: z.string(),
    settings: z.object({
      path: z.string(),
      source: z.enum(["file", "defaults", "disabled", "error"]),
      profileName: z.string().nullable(),
      profile: z.object({
        memoryMax: z.string(),
        tasksMax: z.number().int().nonnegative(),
        cpuQuota: z.string(),
        allowedDomains: z.array(z.string()).max(100),
        allowedDomainsTotal: z.number().int().nonnegative(),
        runtimeReadPathsCount: z.number().int().nonnegative(),
      }).nullable(),
      error: z.string().optional(),
    }),
    workspacesOnDisk: z.number().int().nonnegative().nullable(),
    workspacesError: z.string().optional(),
    active: z.array(z.object({
      sandboxId: z.string().min(1),
      state: sandboxStateSchema,
      refs: z.number().int().nonnegative(),
      startedAt: z.string(),
      workspacePath: z.string(),
      mcpUrl: z.string().nullable(),
      mcpServerName: z.string().nullable(),
      lastError: z.object({ code: z.string().min(1), message: z.string() }).nullable(),
    })),
    starting: z.number().int().nonnegative(),
  }),
});

/** Action 封套里的目标绑定；空句柄固定 `{sessionId:null,bindingRevision:0}`。 */
export const actionTargetSchema = z.object({
  sessionId: z.string().min(1).nullable(),
  bindingRevision: z.number().int().nonnegative(),
});

/** 202 回执：send 额外带 inputId；控制/历史动作带各自的纯数据结果。 */
export const chatActionSuccessSchema = z.object({
  ok: z.literal(true),
  requestId: z.string().min(1),
  chatId: z.string().min(1),
  sessionId: z.string(),
  bindingRevision: z.number().int().nonnegative(),
  inputId: z.string().min(1).optional(),
  accepted: z.boolean().optional(),
  status: z.string().min(1).optional(),
  applied: z.number().int().nonnegative().optional(),
  nextCursor: z.string().nullable().optional(),
});

/**
 * HTTP 错误封套：`disposition` 是唯一可重试性权威。
 *
 * `not_sent` 只由客户端在请求未提交时产生；服务端只返回 `rejected`/`unknown`。
 */
export const chatErrorResponseSchema = z.object({
  ok: z.literal(false),
  code: z.string().min(1),
  message: z.string(),
  disposition: z.enum(["rejected", "unknown", "not_sent"]),
  requestId: z.string().optional(),
  chatId: z.string().optional(),
  createdSessionId: z.string().optional(),
  stage: z.string().optional(),
  incomplete: z.boolean().optional(),
  errors: z.array(z.string()).optional(),
});

export const payloadResponseSchema = z.object({
  ok: z.literal(true),
  payload: z.object({
    id: z.string().min(1),
    bytes: z.number().int().nonnegative(),
    json: z.string(),
  }),
});

export const payloadErrorResponseSchema = z.object({
  ok: z.literal(false),
  code: z.string().min(1),
  message: z.string(),
});

// —— 公开展示 DTO：由 SDK SessionView 物化，不额外持久化。——

const publicErrorSchema = z.object({ code: z.string().min(1), message: z.string() });
const toolPayloadRefSchema = z.object({
  id: z.string().min(1), bytes: z.number().int().nonnegative(), preview: z.string(),
});
export const toolCallSchema = z.object({
  toolCallId: z.string().min(1), turnId: z.string().nullable(), entryId: z.string().optional(),
  name: z.string(), status: z.string().min(1), kind: z.string().optional(),
  permissionId: z.string().optional(),
  createdAt: z.string().nullable(), completedAt: z.string().nullable(),
  error: publicErrorSchema.nullable(),
  arguments: z.unknown().optional(), result: z.unknown().optional(),
  argumentsRef: toolPayloadRefSchema.optional(), resultRef: toolPayloadRefSchema.optional(),
});
const entryBlockSchema = z.discriminatedUnion("type", [
  z.object({ blockId: z.string().min(1), type: z.literal("text"), text: z.string() }),
  z.object({
    blockId: z.string().min(1), type: z.literal("reasoning"), text: z.string(),
    // SDK 只保证 `visibility?: string`；未知值必须透传，不能反过来冻结整份展示态。
    visibility: z.string().optional(),
  }),
  z.object({
    blockId: z.string().min(1), type: z.literal("tool_call"), toolCallId: z.string().min(1),
    tool: toolCallSchema.nullable(), unavailable: z.string().optional(),
  }),
  z.object({ blockId: z.string().min(1), type: z.literal("unknown"), reason: z.string().min(1) }),
]);
export const entrySchema = z.object({
  entryId: z.string().min(1), id: z.string().min(1), kind: z.enum(["message", "tool", "system"]),
  turnId: z.string().min(1), role: z.enum(["user", "assistant"]), status: z.string().min(1),
  messageId: z.string().optional(),
  createdAt: z.string().nullable(), createdAtSource: z.string(), completedAt: z.string().nullable(),
  error: publicErrorSchema.nullable(), blocks: z.array(entryBlockSchema),
  tokenUsage: z.record(z.string(), z.unknown()).optional(),
});
export const turnSchema = z.object({
  turnId: z.string().min(1), status: z.string().min(1), requestId: z.string().optional(),
  startedAt: z.string().nullable(), completedAt: z.string().nullable(),
  error: publicErrorSchema.nullable(), stopReason: z.string().optional(),
  interactionStatus: z.string().min(1),
});
export const planEntrySchema = z.object({ content: z.string(), status: z.string(), activeForm: z.string().optional() });
export const taskSchema = z.object({
  taskId: z.string().min(1), kind: z.string(), title: z.string(), status: z.string().min(1),
  summary: z.string().optional(), isBackground: z.boolean().optional(), taskSubtype: z.string().optional(),
  startedAt: z.string().optional(), completedAt: z.string().optional(), updatedAt: z.string().optional(),
  durationMs: z.number().optional(),
});
/** AskUserQuestion 选项的 wire 值（`value`）与展示文本（`label`）；description 可选。 */
const askUserQuestionOptionSchema = z.object({
  value: z.string().min(1), label: z.string().min(1), description: z.string().optional(),
});
/** AskUserQuestion 单题：id 是 elicitation schema 的属性键（`ask_user_question_<index>`）。 */
const askUserQuestionSchema = z.object({
  id: z.string().min(1), header: z.string(), question: z.string().min(1),
  multiSelect: z.boolean(), options: z.array(askUserQuestionOptionSchema),
});
export const interactionSchema = z.object({
  id: z.string().min(1), kind: z.enum(["permission", "elicitation"]), sessionId: z.string().min(1),
  turnId: z.string().optional(), toolCallId: z.string().optional(),
  status: z.enum(["pending", "resolved", "expired"]),
  createdAt: z.string().nullable(), resolvedAt: z.string().nullable(), outcome: z.string().optional(),
  title: z.string().optional(), message: z.string().optional(), formUnsupported: z.string().optional(),
  variant: z.literal("ask_user").optional(),
  options: z.array(z.object({ optionId: z.string(), name: z.string() })).optional(),
  fields: z.array(z.object({
    key: z.string(), title: z.string(), description: z.string(),
    type: z.enum(["string", "number", "integer", "boolean"]), required: z.boolean(),
    enum: z.array(z.union([z.string(), z.number(), z.boolean()])).optional(),
  })).optional(),
  /** AskUserQuestion 请求的题目；与通用 elicitation `fields` 互斥（来自 SDK view）。 */
  questions: z.array(askUserQuestionSchema).optional(),
});
export const inputQueueSchema = z.object({
  generation: z.string().min(1), revision: z.number(),
  activeRequestId: z.string().optional(),
  items: z.array(z.object({ inputId: z.string().min(1), state: z.string().min(1) })),
});
export const sessionInfoSchema = z.object({
  title: z.string().optional(), model: z.string().optional(), mode: z.string().optional(),
  updatedAt: z.string().optional(), activeRequestId: z.string().optional(),
  activeTurnInteractionStatus: z.string().optional(), compactionStatus: z.string().optional(),
  suspendedTurnId: z.string().optional(), legacyIncomplete: z.boolean().optional(),
  rewindRevision: z.number().optional(),
  modeState: z.record(z.string(), z.unknown()).optional(),
  modelState: z.record(z.string(), z.unknown()).optional(),
  goal: z.record(z.string(), z.unknown()).optional(),
  stateSnapshotMeta: z.record(z.string(), z.unknown()).optional(),
  compaction: z.record(z.string(), z.unknown()).optional(),
  configOptions: z.array(z.record(z.string(), z.unknown())).optional(),
  availableCommands: z.array(z.record(z.string(), z.unknown())).optional(),
});
export const activeTurnSchema = z.object({ turnId: z.string().nullable(), status: z.string().nullable() });

export type EntryDTO = z.infer<typeof entrySchema>;
export type ToolCallDTO = z.infer<typeof toolCallSchema>;
export type TurnDTO = z.infer<typeof turnSchema>;
export type InteractionDTO = z.infer<typeof interactionSchema>;
export type AskUserQuestionDTO = z.infer<typeof askUserQuestionSchema>;
export type MonitorSnapshot = z.infer<typeof monitorSnapshotSchema>;
export type Chat = z.infer<typeof chatSchema>;
export type ChatSummary = z.infer<typeof chatSummarySchema>;
export type ProjectSummary = z.infer<typeof projectSchema>;

/** 编译期守卫：除服务端派生的 `projectId` 外，目录 DTO 与 SDK 条目字段集合必须一致。 */
type AssertSameKeys<A, B> = [Exclude<keyof A, keyof B> | Exclude<keyof B, keyof A>] extends [never]
  ? true
  : { driftedKeys: Exclude<keyof A, keyof B> | Exclude<keyof B, keyof A> };
const _directoryKeysMatchSdk: AssertSameKeys<Omit<ChatSummary, "projectId">, ChatSessionDirectoryEntry> = true;
export type ChatHandleDTO = z.infer<typeof chatHandleSchema>;
export type ActionTargetDTO = z.infer<typeof actionTargetSchema>;
export type ChatActionSuccessDTO = z.infer<typeof chatActionSuccessSchema>;
