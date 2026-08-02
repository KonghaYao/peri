/**
 * 共享类型 — JSON-RPC 2.0 wire types 与 CLI 读取器类型。
 *
 * JSON-RPC 2.0 协议对齐 spec 第 3 节：newline-delimited JSON（每行一条消息）。
 * 引擎相关类型（AgentRunParams 等）直接复用 @claude-code-best/workflow-engine。
 */
import type {
  AgentRunParams,
  JournalEntry,
} from '@claude-code-best/workflow-engine'

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

/** runner → host: execute one agent call */
export type AgentRunRequestParams = AgentRunParams & {
  runId: string
  agentId: number
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
