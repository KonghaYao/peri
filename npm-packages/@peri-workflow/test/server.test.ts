/**
 * server 测试 — handleRequest 的 workflow/start（真实 engine 闭环）、kill、未知方法。
 *
 * 不 mock engine：start 后真实 engine 发出 agent/run，测试以 handleResponse
 * 模拟宿主响应，断言 workflow/done 终态。process.exit 置为 noop
 * （engine 完成后会调用，测试进程不能真退出）。
 */
import { afterAll, describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
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

  test('phase callback is not executed but phase events are emitted', async () => {
    written = []
    const script = `export const meta = { name: 'phase-contract', description: 'phase callback contract' }
phase('marker', async () => {
  await agent('must-not-run')
})
return { phaseUpdated: true }`

    await handleRequest({
      jsonrpc: '2.0',
      id: 2,
      method: 'workflow/start',
      params: { runId: 'srv-phase', cwd: '/tmp', script },
    })

    const done = await waitFor((m) => m.method === 'workflow/done')
    const params = done.params as { status: string; returnValue: { phaseUpdated: boolean } }
    expect(params.status).toBe('completed')
    expect(params.returnValue).toEqual({ phaseUpdated: true })
    expect(written.some((m) => m.method === 'agent/run')).toBe(false)

    const phaseTypes = written
      .filter((m) => m.method === 'progress/event')
      .map((m) => m.params as { type: string; phase?: string })
      .filter((event) => event.type === 'phase_started' || event.type === 'phase_done')
    expect(phaseTypes).toEqual([
      expect.objectContaining({ type: 'phase_started', phase: 'marker' }),
      expect.objectContaining({ type: 'phase_done', phase: 'marker' }),
    ])
  })

  test('resume cache-hit 写入结构化 recovered attempt identity', async () => {
    written = []
    const script = `export const meta = { name: 'resume-demo', description: 'resume test' }
return await agent('hello')`
    const key = createHash('sha256')
      .update('hello\n' + JSON.stringify({ prompt: 'hello' }))
      .digest('hex')

    await handleRequest({
      jsonrpc: '2.0',
      id: 3,
      method: 'workflow/start',
      params: {
        runId: '019-current-run',
        cwd: '/tmp',
        script,
        resumeFromRunId: '018-source-run',
        resume: [{ key, seq: 0, result: { kind: 'ok', output: 'cached', usage: { outputTokens: 1 } } }],
      },
    })

    const append = await waitFor((m) => m.method === 'journal/append')
    const entry = (append.params as { entry: { attempt: Record<string, unknown> } }).entry
    expect(entry.attempt).toEqual({
      runId: '019-current-run',
      journalSeq: 0,
      recoveredFrom: { runId: '018-source-run', journalSeq: 0 },
      consumed: true,
      disposition: 'recovered',
    })
  })

  test('resume dead/skipped cache entry re-executes the live call', async () => {
    written = []
    const script = `export const meta = { name: 'resume-dead', description: 'resume dead test' }
return await agent('retry-me')`

    await handleRequest({
      jsonrpc: '2.0',
      id: 4,
      method: 'workflow/start',
      params: {
        runId: '019-retry-run',
        cwd: '/tmp',
        script,
        resumeFromRunId: '018-dead-run',
        resume: [{ key: 'dead-entry', seq: 0, result: { kind: 'dead', reason: 'runagent-threw' } }],
      },
    })

    const agentReq = await waitFor((m) => m.method === 'agent/run')
    expect(agentReq.params).toMatchObject({ runId: '019-retry-run', agentId: 0, prompt: 'retry-me' })
    handleResponse({
      jsonrpc: '2.0',
      id: agentReq.id as number,
      result: { kind: 'ok', output: 'recovered-live', usage: { outputTokens: 1 } },
    } as never)
    const done = await waitFor((m) => m.method === 'workflow/done')
    expect((done.params as { returnValue: string }).returnValue).toBe('recovered-live')
    expect(written.filter((m) => m.method === 'agent/run')).toHaveLength(1)
    const produced = written
      .filter((m) => m.method === 'journal/append')
      .map((m) => (m.params as { entry: { attempt?: Record<string, unknown> } }).entry)
      .find((entry) => entry.attempt?.disposition === 'produced')
    expect(produced?.attempt).toEqual({
      runId: '019-retry-run',
      journalSeq: 0,
      consumed: true,
      disposition: 'produced',
    })
  })

  test('resumeFromRunId without a journal is a parameter error and starts no agent', async () => {
    for (const resume of [undefined, null, { invalid: true }]) {
      written = []
      await handleRequest({
        jsonrpc: '2.0',
        id: 40,
        method: 'workflow/start',
        params: {
          runId: '019-invalid-resume',
          cwd: '/tmp',
          script: "return 'must-not-run'",
          resumeFromRunId: '018-source-run',
          ...(resume === undefined ? {} : { resume }),
        },
      })
      expect(written).toEqual([{
        jsonrpc: '2.0',
        id: 40,
        error: { code: -32602, message: 'resume must be an array when resumeFromRunId is present' },
      }])
    }
    expect(written.some((message) => message.method === 'agent/run')).toBe(false)
  })

  test('workflow/start：invalid-present budgetTotal 在启动前同步拒绝', async () => {
    const invalid = [null, 0, -1, 1.5, '1', Number.MAX_SAFE_INTEGER + 1]

    for (const [index, budgetTotal] of invalid.entries()) {
      written = []
      await handleRequest({
        jsonrpc: '2.0',
        id: 100 + index,
        method: 'workflow/start',
        params: {
          runId: `invalid-budget-${index}`,
          cwd: '/tmp',
          script: "return 'must-not-run'",
          budgetTotal,
        },
      })

      expect(written).toEqual([
        {
          jsonrpc: '2.0',
          id: 100 + index,
          error: {
            code: -32602,
            message: `budgetTotal must be an integer between 1 and ${Number.MAX_SAFE_INTEGER}`,
          },
        },
      ])
    }
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
