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
export const WORKFLOW_PROTOCOL_VERSION = 1
export const WORKFLOW_BUILD_ID = '@peri-code/workflow@0.2.0'

/** host → runner: start a workflow */
export type WorkflowStartParams = {
  runId: string
  cwd: string
  budgetTotal: number | null
  resume?: JournalEntry[]
  script: string
  args?: unknown
  maxConcurrency?: number
}

/**
 * runner → host: 执行一次 agent 调用的请求参数（wire 字段）。
 *
 * 显式声明而非透传引擎 `AgentRunParams`：字段与 Rust 侧
 * `protocol.rs::AgentRunParams` 逐字段对齐（camelCase wire 命名），
 * 避免引擎类型升级导致两侧静默分叉。
 */
export type AgentRunRequestParams = {
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

// ─── CLI 读取器类型 ────────────────────────────────────────

/** `.claude/workflow-runs/<runId>/state.json` 的结构 */
export interface RunState {
  run_id: string
  workflow_name: string
  status: string
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
