/**
 * JSON-RPC 模式启动 — 监听 stdin 的 newline-delimited JSON 并分发。
 */
import * as readline from 'readline'
import { handleResponse, handleTaskResult, handleToolResult } from './rpc'
import type { JsonRpcMessage, JsonRpcRequest, JsonRpcResponse } from './types'
import { handleRequest } from './server'

export function startJsonRpc(): void {
  const rl = readline.createInterface({ input: process.stdin })

  rl.on('line', (line: string) => {
    let msg: JsonRpcMessage
    try {
      msg = JSON.parse(line)
    } catch {
      return // ignore invalid JSON lines
    }

    // Is this a response to a pending request?
    if (
      'id' in msg &&
      msg.id !== undefined &&
      ('result' in msg || 'error' in msg)
    ) {
      handleResponse(msg as JsonRpcResponse)
      return
    }

    // 宿主通知：agent/result 结算任务等待表（P6 任务信道）。
    if ('method' in msg && msg.method === 'agent/result') {
      handleTaskResult(msg as { params?: { taskId?: unknown; result?: unknown } })
      return
    }

    // 宿主通知：tool/result 结算脚本工具调用等待表（issue #407 阶段 1）。
    if ('method' in msg && msg.method === 'tool/result') {
      handleToolResult(msg as { params?: { taskId?: unknown; runId?: unknown; outcome?: unknown } })
      return
    }

    // Is this an incoming request?
    if ('method' in msg && msg.method) {
      void handleRequest(msg as JsonRpcRequest)
    }
  })
}
