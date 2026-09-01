/**
 * server 测试 — handleRequest 的 workflow/start（真实 engine 闭环）、kill、未知方法。
 *
 * 不 mock engine：start 后真实 engine 发出 agent/run，测试以 handleResponse
 * 模拟宿主响应，断言 workflow/done 终态。process.exit 置为 noop
 * （engine 完成后会调用，测试进程不能真退出）。
 */
import { afterAll, describe, expect, test } from 'bun:test'
import { handleRequest } from '../src/server'
import { handleResponse, setOutWriter } from '../src/rpc'

// ─── 进程级 patch ──────────────────────────────────────────

const origExit = process.exit
let written: Record<string, unknown>[] = []

process.exit = (() => {}) as typeof process.exit
setOutWriter((line) => {
  written.push(JSON.parse(line) as Record<string, unknown>)
})

afterAll(() => {
  process.exit = origExit
  setOutWriter((line) => process.stdout.write(line))
})

async function waitFor(
  pred: (m: Record<string, unknown>) => boolean,
  timeoutMs = 10000
): Promise<Record<string, unknown>> {
  const deadline = Date.now() + timeoutMs
  for (;;) {
    const found = written.find(pred)
    if (found) return found
    if (Date.now() > deadline) {
      throw new Error(`waitFor timeout; seen=${JSON.stringify(written)}`)
    }
    await new Promise((r) => setTimeout(r, 25))
  }
}

describe('handleRequest', () => {
  test('workflow/start：完整执行（真实 engine）→ workflow/done', async () => {
    written = []
    const script = `export const meta = { name: 'srv-demo', description: 'srv test' }
phase('run')
const r = await agent('hello')
return { answer: r }`

    await handleRequest({
      jsonrpc: '2.0',
      id: 1,
      method: 'workflow/start',
      params: { runId: 'srv-1', cwd: '/tmp', script, budgetTotal: Number.MAX_SAFE_INTEGER },
    })

    // start 同步响应
    const startResp = written.find((m) => m.id === 1)
    expect(startResp?.result).toEqual({
      ok: true,
      protocolVersion: 1,
      buildId: '@peri-code/workflow@0.2.0',
    })

    // 真实 engine 发 agent/run → 模拟宿主响应
    const agentReq = await waitFor((m) => m.method === 'agent/run')
    handleResponse({
      jsonrpc: '2.0',
      id: agentReq.id as number,
      result: { kind: 'ok', output: 'srv-out', usage: { outputTokens: 5 } },
    } as never)

    // 终态
    const done = await waitFor((m) => m.method === 'workflow/done')
    const params = done.params as { status: string; returnValue: { answer: string } }
    expect(params.status).toBe('completed')
    expect(params.returnValue).toEqual({ answer: 'srv-out' })

    // 事件链
    const types = written
      .filter((m) => m.method === 'progress/event')
      .map((m) => (m.params as { type: string }).type)
    expect(types).toEqual(
      expect.arrayContaining(['run_started', 'phase_started', 'agent_started', 'agent_done', 'phase_done', 'run_done'])
    )
    expect(written.some((m) => m.method === 'journal/append')).toBe(true)
  })

  test('workflow/kill：响应 ok', async () => {
    written = []
    await handleRequest({ jsonrpc: '2.0', id: 2, method: 'workflow/kill' })
    const msg = written.find((m) => m.id === 2)
    expect(msg?.result).toEqual({ ok: true })
  })

  test('未知方法：返回 -32601 错误', async () => {
    written = []
    await handleRequest({ jsonrpc: '2.0', id: 9, method: 'nope/method' })
    const msg = written.find((m) => m.id === 9)
    expect((msg?.error as { code: number }).code).toBe(-32601)
  })
})
