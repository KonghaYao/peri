/**
 * JSON-RPC 2.0 传输层 — 请求/响应/通知的发送与 pending 管理。
 *
 * stdout 传 JSON-RPC（每行一条消息），stderr 留给 console.error。
 */
import type { Writable } from 'node:stream'
import type {
  JsonRpcId,
  JsonRpcMessage,
  JsonRpcResponse,
} from './types'

let reqId = 100
let _msgSeq = 0

/** 输出目标（默认 stdout；测试可注入收集器，避免 patch 全局对象） */
let writeOut: (line: string) => void = (line) => process.stdout.write(line)

/** 替换输出目标。仅测试用：传入收集器捕获消息，而不是 patch process.stdout */
export function setOutWriter(fn: (line: string) => void): void {
  writeOut = fn
}

const pending = new Map<
  JsonRpcId,
  { resolve: (value: unknown) => void; reject: (error: unknown) => void }
>()

export function send(msg: JsonRpcMessage): void {
  _msgSeq++
  writeOut(JSON.stringify(msg) + '\n')
}

/** 等待 stdout 排空——使用 Node.js 内置 writableNeedDrain 避免竞态 */
export function waitDrain(): Promise<void> {
  return new Promise((resolve) => {
    if ((process.stdout as Writable).writableNeedDrain) {
      process.stdout.once('drain', resolve)
    } else {
      resolve()
    }
  })
}

export function rpcRequest(method: string, params: unknown): Promise<unknown> {
  const id = reqId++
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject })
    send({ jsonrpc: '2.0', id, method, params })
  })
}

export function rpcNotify(method: string, params: unknown): void {
  send({ jsonrpc: '2.0', method, params })
}

// ─── agent/result 等待表（P6：任务结果经通知投递，不再有请求/响应）────

/** 已发布任务等待表（taskId → pending resolver）。 */
const pendingTasks = new Map<string, { resolve: (value: unknown) => void }>()

/** 登记一个已发布任务的等待项（`agent/task` 通知发出前调用）。 */
export function registerTask(taskId: string): Promise<unknown> {
  return new Promise((resolve) => {
    pendingTasks.set(taskId, { resolve })
  })
}

/**
 * 处理宿主 `agent/result` 通知：按 taskId 结算等待项。
 *
 * 幂等：未知 taskId（迟到/重复投递、或已由 run 结束清空）直接忽略。
 */
export function handleTaskResult(msg: {
  params?: { taskId?: unknown; result?: unknown }
}): void {
  const taskId = msg.params?.taskId
  if (typeof taskId !== 'string') return
  const entry = pendingTasks.get(taskId)
  if (!entry) return
  pendingTasks.delete(taskId)
  entry.resolve(msg.params?.result)
}

/**
 * 结算全部未完成任务（run 结束/kill 收口；不残留悬挂 promise）。
 *
 * 以取消编码结算（`dead { reason: 'killed' }`），与宿主取消路径同构 ——
 * adapter 据此抛 `WorkflowAbortedError`，与旧链 `-32000` 错误响应等价。
 */
export function rejectPendingTasks(message: string): void {
  for (const [, entry] of pendingTasks) {
    entry.resolve({ kind: 'dead', reason: 'killed', detail: message })
  }
  pendingTasks.clear()
}

export function handleResponse(msg: JsonRpcResponse): void {
  const entry = pending.get(msg.id)
  if (!entry) return
  pending.delete(msg.id)
  if (msg.error) {
    entry.reject(msg.error)
  } else {
    entry.resolve(msg.result)
  }
}
