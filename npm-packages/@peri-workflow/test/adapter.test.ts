/**
 * adapter 测试 — rpcAdapter.run 的三条路径（ok / cancelled / 迟到投递）。
 *
 * 不 mock 模块：走真实 rpc 传输闭环——setOutWriter 捕获 agent/task 通知，
 * handleTaskResult 模拟宿主 agent/result 回投（与 e2e 黑盒同构，但单元级驱动）。
 */
import { describe, expect, test, afterEach } from 'bun:test'
import * as engine from '@claude-code-best/workflow-engine'
import type { AgentAdapterContext } from '@claude-code-best/workflow-engine'
import { rpcAdapter } from '../src/adapter'
import { handleTaskResult, setOutWriter } from '../src/rpc'

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

/** 等待下一个 agent/task 通知并模拟宿主回投 agent/result */
function respond(agentTask: Record<string, unknown>, result: unknown): void {
  handleTaskResult({
    params: {
      taskId: (agentTask.params as { taskId: string }).taskId,
      result,
    },
  })
}

function waitAgentTask(): Record<string, unknown> {
  const req = written.find((m) => m.method === 'agent/task')
  if (!req) throw new Error(`no agent/task sent; seen=${JSON.stringify(written)}`)
  return req
}

const ctx: AgentAdapterContext = {
  runId: 'run-1',
  agentId: 3,
  host: null as never,
  signal: new AbortController().signal,
}

describe('rpcAdapter.run', () => {
  test('ok 路径：透传宿主结果并带上 runId/agentId/taskId', async () => {
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
    const task = waitAgentTask()
    expect(task.params).toMatchObject({
      runId: 'run-1',
      agentId: 3,
      prompt: 'hello',
      model: 'sonnet',
      maxTokens: 4096,
      agentType: 'web-researcher',
      allowedTools: ['WebSearch'],
      phase: 'fix',
    })
    expect(typeof (task.params as { taskId: string }).taskId).toBe('string')
    respond(task, { kind: 'ok', output: 'mock result', usage: { outputTokens: 7 } })
    await expect(p).resolves.toEqual({
      kind: 'ok',
      output: 'mock result',
      usage: { outputTokens: 7 },
    })
  })

  test('取消编码（dead/killed）→ 抛 WorkflowAbortedError', async () => {
    capture()
    const p = rpcAdapter.run({ prompt: 'x' }, ctx)
    const task = waitAgentTask()
    respond(task, { kind: 'dead', reason: 'killed', detail: 'workflow run killed' })
    await expect(p).rejects.toBeInstanceOf(engine.WorkflowAbortedError)
  })

  test('普通 dead 结果 → 透传（不抛 aborted）', async () => {
    capture()
    const p = rpcAdapter.run({ prompt: 'x' }, ctx)
    const task = waitAgentTask()
    respond(task, { kind: 'dead', reason: 'runagent-threw', detail: 'boom' })
    await expect(p).resolves.toEqual({
      kind: 'dead',
      reason: 'runagent-threw',
      detail: 'boom',
    })
  })

  test('迟到/未知 taskId 投递被忽略（幂等）', async () => {
    capture()
    const p = rpcAdapter.run({ prompt: 'x' }, ctx)
    const task = waitAgentTask()
    // 首次投递结算
    respond(task, { kind: 'ok', output: 'first', usage: { outputTokens: 1 } })
    await expect(p).resolves.toEqual({
      kind: 'ok',
      output: 'first',
      usage: { outputTokens: 1 },
    })
    // 重复投递：等待表已清空，不得抛错也不得改变已结算结果
    expect(() => respond(task, { kind: 'dead', reason: 'killed' })).not.toThrow()
  })
})
