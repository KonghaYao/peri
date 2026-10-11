/**
 * 共享类型 — JSON-RPC 2.0 wire types 与 CLI 读取器类型。
 *
 * JSON-RPC 2.0 协议对齐 spec 第 3 节：newline-delimited JSON（每行一条消息）。
 *
 * ⚠ 跨侧契约：本文件的 wire 字段（WorkflowStartParams / AgentRunRequestParams 等）
 * 与 Rust 侧 `peri-workflow/src/protocol.rs` 保持同步，变更须两侧一致
 * （Rust 侧文件顶部有对应注释）。
 * CLI 领域类型（RunState / AgentResult）与宿主落盘格式对齐，见 DESIGN.md
 * 「运行结果落盘格式」节（Rust `journal.rs` 写入 ↔ `reader.ts` 读取，双向对齐）。
 *
 * 引擎相关类型（JournalEntry 等）复用 @claude-code-best/workflow-engine。
 */
import type { JournalEntry } from '@claude-code-best/workflow-engine'

// ─── JSON-RPC wire types ───────────────────────────────────

export type JsonRpcId = string | number

export type JsonRpcRequest = {
  jsonrpc: '2.0'
  id?: JsonRpcId
  method: string
  params?: unknown
}

export type JsonRpcResponse = {
  jsonrpc: '2.0'
  id: JsonRpcId
  result?: unknown
  error?: { code: number; message: string; data?: unknown }
}

export type JsonRpcNotification = {
  jsonrpc: '2.0'
  method: string
  params?: unknown
}

export type JsonRpcMessage = JsonRpcRequest | JsonRpcResponse | JsonRpcNotification

// ─── RPC method signatures ─────────────────────────────────

/** Rust host 与 Node artifact 必须共同实现的启动协议。 */
export const WORKFLOW_PROTOCOL_VERSION = 2
export const WORKFLOW_BUILD_ID = '@peri-code/workflow@0.3.0'

export type WorkflowLimits = {
  maxAgents?: number
  maxToolCalls?: number
  maxElapsedMs?: number
}

/** host → runner: start a workflow */
export type WorkflowStartParams = {
  runId: string
  cwd: string
  budgetTotal?: number
  limits?: WorkflowLimits
  resumeFromRunId?: string
  resume?: JournalEntry[]
  script: string
  args?: unknown
  maxConcurrency?: number
}

/**
 * runner → host: agent 任务发布（通知；P6 wire）。
 *
 * 显式声明而非透传引擎 `AgentRunParams`：字段与 Rust 侧
 * `protocol.rs::AgentTaskParams` 逐字段对齐（camelCase wire 命名），
 * 避免引擎类型升级导致两侧静默分叉。
 *
 * `taskId` 作用域 = 单次 run；宿主回填经 `agent/result` 通知（按 taskId 去重）。
 */
export type AgentTaskParams = {
  taskId: string
  runId: string
  agentId: number
  prompt: string
  schema?: object
  model?: string
  maxTokens?: number
  agentType?: string
  isolation?: 'worktree'
  allowedTools?: string[]
  label?: string
  phase?: string
}

/**
 * host → runner: agent 任务结果投递（通知；P6 wire）。
 *
 * 字段与 Rust 侧 `protocol.rs::AgentResultParams` 对齐；`result` 为引擎
 * `AgentRunResult`（`ok` / `skipped` / `dead`）。取消编码为
 * `dead { reason: 'killed' }`，adapter 按引擎约定抛 `WorkflowAbortedError`。
 */
export type AgentResultParams = {
  runId: string
  taskId: string
  result: unknown
}

// ─── CLI 读取器类型 ────────────────────────────────────────

export type WorkflowWriteIntent =
  | { kind: 'read_only' }
  | {
      kind: 'write'
      repo_root: string
      cwd: string
      path_allowlist: string[]
      head_may_change?: boolean
      commit_required?: boolean
    }

export type WorkflowExecutionStatus = 'running' | 'completed' | 'failed' | 'killed' | 'unknown'
export type WorkflowAcceptanceStatus = 'passed' | 'failed' | 'unknown'
export type WorkflowPostProcessingStatus = 'not_required' | 'passed' | 'failed' | 'blocked' | 'unknown'
/** 内核只产出 `blocked` / `unknown`；历史 state.json 可能写着 `deliverable`，按 `unknown` 看待。 */
export type WorkflowDeliveryStatus = 'blocked' | 'unknown'

export type AttemptDisposition = 'produced' | 'recovered' | 'rejected'

export type WorkflowAttempt = {
  runId: string
  /** Absent when the engine journal callback cannot prove the originating agent identity. */
  agentId?: number
  journalSeq: number
  recoveredFrom?: { runId: string; agentId?: number; journalSeq: number }
  consumed: boolean
  disposition: AttemptDisposition
}

export type WorkflowJournalEntry = JournalEntry & { attempt?: WorkflowAttempt }

/** `.claude/workflow-runs/<runId>/state.json` 的结构 */
export interface RunState {
  run_id: string
  workflow_name: string
  status: string
  /** Legacy completed only proves execution completion, never deliverability. */
  execution_status?: WorkflowExecutionStatus
  acceptance_status?: WorkflowAcceptanceStatus
  post_processing_status?: WorkflowPostProcessingStatus
  delivery_status?: WorkflowDeliveryStatus
  limits?: WorkflowLimits
  budget_total?: number
  /** Exact workflow script arguments; absent in legacy state snapshots. */
  args?: unknown
  /** Persisted concurrency; absent in legacy snapshots (Rust defaults to 3). */
  max_concurrency?: number
  attempts?: WorkflowAttempt[]
  return_value?: unknown
  script?: string
  started_at?: string
  finished_at?: string
  error?: string
}

/** journal.jsonl 中单条 agent 结果的展平视图 */
export interface AgentResult {
  seq: number
  kind: 'ok' | 'skipped' | 'dead'
  output?: unknown
  tokens?: number
  tools?: number
  durationMs?: number
  phase?: string
  reason?: string
  detail?: string
}
