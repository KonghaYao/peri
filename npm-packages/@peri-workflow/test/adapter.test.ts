/**
 * adapter 测试 — rpcAdapter.run 的三条路径（ok / aborted / 其他错误）。
 *
 * 不 mock 模块：走真实 rpc 传输闭环——setOutWriter 捕获 agent/run 请求，
 * handleResponse 模拟宿主响应（与 e2e 黑盒同构，但单元级驱动）。
 */
import { describe, expect, test, afterEach } from 'bun:test'
import * as engine from '@claude-code-best/workflow-engine'
import type { AgentAdapterContext } from '@claude-code-best/workflow-engine'
import { rpcAdapter } from '../src/adapter'
import { handleResponse, setOutWriter } from '../src/rpc'

let written: Record<string, unknown>[] = []

function capture(): void {
  written = []
  setOutWriter((line) => {
    written.push(JSON.parse(line) as Record<string, unknown>)
  })
}

afterEach(() => {
  setOutWriter((line) => process.stdout.write(line))
})

/** 等待下一个 agent/run 请求并模拟宿主响应 */
function respond(agentReq: Record<string, unknown>, result: unknown): void {
  handleResponse({
    jsonrpc: '2.0',
    id: agentReq.id as number,
    result,
  } as never)
}

function waitAgentReq(): Record<string, unknown> {
  const req = written.find((m) => m.method === 'agent/run')
  if (!req) throw new Error(`no agent/run sent; seen=${JSON.stringify(written)}`)
  return req
}

const ctx: AgentAdapterContext = {
  runId: 'run-1',
  agentId: 3,
  host: null as never,
  signal: new AbortController().signal,
}

describe('rpcAdapter.run', () => {
  test('ok 路径：透传宿主结果并带上 runId/agentId', async () => {
    capture()
    const p = rpcAdapter.run(
      {
        prompt: 'hello',
        model: 'sonnet',
        maxTokens: 4096,
        agentType: 'web-researcher',
        allowedTools: ['WebSearch'],
        phase: 'fix',
      },
      ctx
    )
    const req = waitAgentReq()
    expect(req.params).toMatchObject({
      runId: 'run-1',
      agentId: 3,
      prompt: 'hello',
      model: 'sonnet',
      maxTokens: 4096,
      agentType: 'web-researcher',
      allowedTools: ['WebSearch'],
      phase: 'fix',
    })
    respond(req, { kind: 'ok', output: 'mock result', usage: { outputTokens: 7 } })
    await expect(p).resolves.toEqual({
      kind: 'ok',
      output: 'mock result',
      usage: { outputTokens: 7 },
    })
  })

  test('-32000 错误 → 抛 WorkflowAbortedError', async () => {
    capture()
    const p = rpcAdapter.run({ prompt: 'x' }, ctx)
    const req = waitAgentReq()
    handleResponse({
      jsonrpc: '2.0',
      id: req.id as number,
      error: { code: -32000, message: 'aborted' },
    } as never)
    await expect(p).rejects.toBeInstanceOf(engine.WorkflowAbortedError)
  })

  test('其他错误 → dead(runagent-threw)', async () => {
    capture()
    const p = rpcAdapter.run({ prompt: 'x' }, ctx)
    const req = waitAgentReq()
    handleResponse({
      jsonrpc: '2.0',
      id: req.id as number,
      error: { code: -1, message: 'boom' },
    } as never)
    await expect(p).resolves.toEqual({
      kind: 'dead',
      reason: 'runagent-threw',
      detail: '[object Object]', // String(err) 对非 Error 对象的序列化
    })
  })
})
