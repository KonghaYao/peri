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
