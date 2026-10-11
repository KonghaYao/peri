/**
 * rpc 传输层测试 — send / rpcRequest / rpcNotify / handleResponse / 任务等待表 / waitDrain。
 *
 * 通过 setOutWriter 注入收集器捕获输出（模块级隔离，不影响其他测试文件）。
 */
import { describe, expect, test, afterEach } from 'bun:test'
import {
  handleResponse,
  handleTaskResult,
  registerTask,
  rejectPendingTasks,
  rpcNotify,
  rpcRequest,
  send,
  setOutWriter,
  waitDrain,
} from '../src/rpc'

// ─── 输出收集 helper ───────────────────────────────────────

let written: string[] = []

function captureStdout(): void {
  written = []
  setOutWriter((line) => {
    written.push(line)
  })
}

function restoreStdout(): void {
  setOutWriter((line) => process.stdout.write(line))
}

afterEach(restoreStdout)

// ─── send / rpcNotify ──────────────────────────────────────

describe('send / rpcNotify', () => {
  test('send 写出单行 JSON + 换行', () => {
    captureStdout()
    send({ jsonrpc: '2.0', method: 'progress/event', params: { type: 'run_started' } })
    expect(written).toHaveLength(1)
    expect(JSON.parse(written[0])).toEqual({
      jsonrpc: '2.0',
      method: 'progress/event',
      params: { type: 'run_started' },
    })
    expect(written[0].endsWith('\n')).toBe(true)
  })

  test('rpcNotify 发送 notification（无 id）', () => {
    captureStdout()
    rpcNotify('journal/append', { runId: 'r1' })
    const msg = JSON.parse(written[0])
    expect(msg.method).toBe('journal/append')
    expect(msg.id).toBeUndefined()
  })
})

// ─── rpcRequest / handleResponse ───────────────────────────

describe('rpcRequest / handleResponse', () => {
  test('请求带自增 id，响应结果 resolve', async () => {
    captureStdout()
    const p = rpcRequest('some/request', { prompt: 'hi' })
    const sent = JSON.parse(written[0])
    expect(sent.method).toBe('some/request')
    expect(typeof sent.id).toBe('number')
    expect(sent.params).toEqual({ prompt: 'hi' })

    handleResponse({ jsonrpc: '2.0', id: sent.id, result: { kind: 'ok', output: 'x' } })
    await expect(p).resolves.toEqual({ kind: 'ok', output: 'x' })
  })

  test('响应 error 时 reject', async () => {
    captureStdout()
    const p = rpcRequest('some/request', {})
    const sent = JSON.parse(written[0])
    handleResponse({ jsonrpc: '2.0', id: sent.id, error: { code: -32601, message: 'nope' } })
    await expect(p).rejects.toEqual({ code: -32601, message: 'nope' })
  })

  test('未知 id 的响应被忽略（不 panic）', () => {
    captureStdout()
    expect(() => handleResponse({ jsonrpc: '2.0', id: 9999, result: 1 })).not.toThrow()
  })
})

// ─── agent/result 任务等待表（P6 任务信道）──────────────────

describe('registerTask / handleTaskResult / rejectPendingTasks', () => {
  test('按 taskId 结算等待项', async () => {
    const p = registerTask('run-1:t-1')
    handleTaskResult({ params: { taskId: 'run-1:t-1', result: { kind: 'ok', output: 'x' } } })
    await expect(p).resolves.toEqual({ kind: 'ok', output: 'x' })
  })

  test('未知/重复 taskId 的投递被忽略（幂等）', async () => {
    const p = registerTask('run-1:t-2')
    handleTaskResult({ params: { taskId: 'run-1:other', result: { kind: 'ok' } } })
    expect(() => handleTaskResult({ params: { taskId: 'run-1:t-2', result: { kind: 'ok' } } })).not.toThrow()
    await expect(p).resolves.toEqual({ kind: 'ok' })
    // 已结算后再投递：不得抛错
    expect(() => handleTaskResult({ params: { taskId: 'run-1:t-2', result: { kind: 'dead' } } })).not.toThrow()
  })

  test('rejectPendingTasks 以取消编码结算全部等待项', async () => {
    const p1 = registerTask('run-1:t-3')
    const p2 = registerTask('run-1:t-4')
    rejectPendingTasks('workflow killed by host')
    await expect(p1).resolves.toEqual({
      kind: 'dead',
      reason: 'killed',
      detail: 'workflow killed by host',
    })
    await expect(p2).resolves.toEqual({
      kind: 'dead',
      reason: 'killed',
      detail: 'workflow killed by host',
    })
  })
})

// ─── waitDrain ─────────────────────────────────────────────

/** bun 的 process.stdout.writableNeedDrain 是只读属性，用 defineProperty 覆盖 */
function setNeedDrain(value: boolean): () => void {
  const w = process.stdout as unknown as Record<string, unknown>
  const desc = Object.getOwnPropertyDescriptor(process.stdout, 'writableNeedDrain')
  Object.defineProperty(process.stdout, 'writableNeedDrain', {
    value,
    configurable: true,
    writable: true,
  })
  return () => {
    if (desc) {
      Object.defineProperty(process.stdout, 'writableNeedDrain', desc)
    } else {
      delete w.writableNeedDrain
    }
  }
}

describe('waitDrain', () => {
  test('writableNeedDrain=false 时立即 resolve', async () => {
    const restore = setNeedDrain(false)
    try {
      await expect(waitDrain()).resolves.toBeUndefined()
    } finally {
      restore()
    }
  })

  test('writableNeedDrain=true 时等待 drain 事件', async () => {
    const restore = setNeedDrain(true)
    try {
      const p = waitDrain()
      setTimeout(() => (process.stdout as unknown as { emit: (e: string) => boolean }).emit('drain'), 0)
      await expect(p).resolves.toBeUndefined()
    } finally {
      restore()
    }
  })
})
