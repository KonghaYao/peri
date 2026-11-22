/**
 * tool() 测试 — 脚本工具调用信道（issue #407 阶段 1）与记录回放（阶段 2）。
 *
 * 走真实 rpc 传输闭环：setOutWriter 捕获 `tool/task` / `tool/replayed` 通知，
 * handleToolResult 模拟宿主 `tool/result` 回投；不 spawn 进程、不 mock engine。
 */
import { afterEach, describe, expect, test } from 'bun:test'
import { handleRequest } from '../src/server'
import {
  handleToolResult,
  registerToolCall,
  rejectPendingToolCalls,
  setOutWriter,
} from '../src/rpc'
import {
  configureToolReplay,
  installToolGlobal,
  noteAgentCompleted,
  noteJournalReset,
  resetToolReplayState,
  tool,
  uninstallToolGlobal,
  WorkflowToolError,
} from '../src/tool'
import type { ToolJournalEntry } from '../src/types'

type ToolTask = {
  taskId: string
  runId: string
  toolName: string
  arguments: Record<string, unknown>
  toolSeq: number
  agentSeq: number
}

let written: Record<string, unknown>[] = []

function capture(): void {
  written = []
  setOutWriter((line) => {
    written.push(JSON.parse(line) as Record<string, unknown>)
  })
}

afterEach(() => {
  uninstallToolGlobal()
  resetToolReplayState()
  setOutWriter((line) => process.stdout.write(line))
})

function toolTasks(): Record<string, unknown>[] {
  return written.filter((m) => m.method === 'tool/task')
}

function taskParams(index = 0): ToolTask {
  const msg = toolTasks()[index]
  if (!msg) throw new Error(`no tool/task sent; seen=${JSON.stringify(written)}`)
  return msg.params as ToolTask
}

function respond(taskId: string, outcome: unknown, runId = 'run-tool'): void {
  handleToolResult({ params: { taskId, runId, outcome } })
}

async function expectToolError(promise: Promise<unknown>): Promise<WorkflowToolError> {
  try {
    await promise
  } catch (error) {
    if (!(error instanceof WorkflowToolError)) {
      throw new Error(`expected WorkflowToolError, got ${String(error)}`)
    }
    return error
  }
  throw new Error('expected tool() to reject')
}

// ─── 回放夹具（issue #407 阶段 2）──────────────────────────

function recorded(overrides: Partial<ToolJournalEntry> = {}): ToolJournalEntry {
  return {
    toolSeq: 0,
    agentSeq: 0,
    taskId: 'tool-1',
    toolName: 'Read',
    arguments: { file_path: '/a' },
    outcome: { kind: 'ok', content: 'recorded' },
    ...overrides,
  }
}

function replayedNotifies(): Record<string, unknown>[] {
  return written.filter((m) => m.method === 'tool/replayed')
}

function configureReplay(entries: ToolJournalEntry[], prefix = 0): void {
  resetToolReplayState()
  configureToolReplay(entries, prefix)
}

// ─── 发布形状 ──────────────────────────────────────────────

describe('tool/task 发布', () => {
  test('精确字段 + 通知无 id + taskId 在 run 内唯一', async () => {
    capture()
    installToolGlobal('run-tool')

    const first = tool('Read', { file_path: '/abs/path' })
    const second = tool('Bash', { command: 'pwd' })

    expect(written).toHaveLength(2)
    const [firstMsg, secondMsg] = written
    expect('id' in firstMsg).toBe(false)
    expect(firstMsg).toEqual({
      jsonrpc: '2.0',
      method: 'tool/task',
      params: {
        taskId: expect.any(String),
        runId: 'run-tool',
        toolName: 'Read',
        arguments: { file_path: '/abs/path' },
        toolSeq: 0,
        agentSeq: 0,
      },
    })
    expect(secondMsg.params).toMatchObject({
      runId: 'run-tool',
      toolName: 'Bash',
      arguments: { command: 'pwd' },
      toolSeq: 1,
      agentSeq: 0,
    })

    const firstTask = taskParams(0)
    const secondTask = taskParams(1)
    expect(firstTask.taskId).toMatch(/^tool-\d+$/)
    expect(secondTask.taskId).not.toBe(firstTask.taskId)

    respond(firstTask.taskId, { kind: 'ok', content: 'read-ok' })
    respond(secondTask.taskId, { kind: 'ok', content: 'bash-ok' })
    await expect(first).resolves.toBe('read-ok')
    await expect(second).resolves.toBe('bash-ok')
  })

  test('省略 arguments 时发布空对象', async () => {
    capture()
    installToolGlobal('run-tool')
    const p = tool('WorkflowList')
    expect(taskParams().arguments).toEqual({})
    respond(taskParams().taskId, { kind: 'ok', content: 'ok' })
    await expect(p).resolves.toBe('ok')
  })
})

// ─── 返回形态 ──────────────────────────────────────────────

describe('tool() 返回形态', () => {
  test('structuredContent 优先，文本兜底（含 null 等价缺省）', async () => {
    capture()
    installToolGlobal('run-tool')

    const structured = tool('Read', { file_path: '/a' })
    respond(taskParams(0).taskId, {
      kind: 'ok',
      content: 'text',
      structuredContent: { lines: 3 },
    })
    await expect(structured).resolves.toEqual({ lines: 3 })

    const textOnly = tool('Read', { file_path: '/b' })
    respond(taskParams(1).taskId, { kind: 'ok', content: 'plain text' })
    await expect(textOnly).resolves.toBe('plain text')

    const nullStructured = tool('Read', { file_path: '/c' })
    respond(taskParams(2).taskId, {
      kind: 'ok',
      content: 'fallback text',
      structuredContent: null,
    })
    await expect(nullStructured).resolves.toBe('fallback text')
  })

  test('>1 MiB 文本结果不被截断', async () => {
    capture()
    installToolGlobal('run-tool')
    const big = 'x'.repeat(1024 * 1024 + 512)

    const p = tool('Read', { file_path: '/big' })
    respond(taskParams().taskId, { kind: 'ok', content: big })
    const result = await p

    expect(typeof result).toBe('string')
    expect(result).toBe(big)
    expect((result as string).length).toBe(big.length)
  })
})

// ─── 错误语义 ──────────────────────────────────────────────

describe('tool() 错误语义', () => {
  test('error outcome → WorkflowToolError 保留 code/tool/message/details', async () => {
    capture()
    installToolGlobal('run-tool')

    const p = tool('Bash', { command: 'rm -rf /tmp/x' })
    respond(taskParams().taskId, {
      kind: 'error',
      code: 'approval_unavailable',
      tool: 'Bash',
      message: 'no online approval endpoint',
      details: { servers: [] },
    })

    const error = await expectToolError(p)
    expect(error).toBeInstanceOf(Error)
    expect(error.name).toBe('WorkflowToolError')
    expect(error.code).toBe('approval_unavailable')
    expect(error.tool).toBe('Bash')
    expect(error.message).toBe('no online approval endpoint')
    expect(error.details).toEqual({ servers: [] })
  })

  test('参数形状拒绝（数组/null/原始值）不发任务', async () => {
    capture()
    installToolGlobal('run-tool')

    for (const args of [[], null, 42, 'raw', true]) {
      const error = await expectToolError(tool('Read', args as never))
      expect(error.code).toBe('invalid_arguments')
      expect(error.tool).toBe('Read')
    }
    for (const name of ['', '   ', null as never]) {
      const error = await expectToolError(tool(name))
      expect(error.code).toBe('invalid_arguments')
    }
    expect(toolTasks()).toHaveLength(0)
  })

  test('未安装时 tool() 以 tool_unavailable 拒绝', async () => {
    capture()
    const error = await expectToolError(tool('Read', { file_path: '/a' }))
    expect(error.code).toBe('tool_unavailable')
    expect(toolTasks()).toHaveLength(0)
  })

  test('malformed outcome → protocol_error', async () => {
    capture()
    installToolGlobal('run-tool')
    const p = tool('Read', { file_path: '/a' })
    respond(taskParams().taskId, { content: 'missing kind' })
    const error = await expectToolError(p)
    expect(error.code).toBe('protocol_error')
  })
})

// ─── 等待表与收口 ──────────────────────────────────────────

describe('tool/result 等待表', () => {
  test('未知 taskId 只警告，不影响 pending', async () => {
    capture()
    installToolGlobal('run-tool')
    const p = tool('Read', { file_path: '/a' })
    const { taskId } = taskParams()

    let settled = false
    void p.then(
      () => {
        settled = true
      },
      () => {
        settled = true
      },
    )

    const originalError = console.error
    const warnings: unknown[][] = []
    console.error = (...args: unknown[]) => {
      warnings.push(args)
    }
    try {
      handleToolResult({
        params: { taskId: 'tool-unknown', outcome: { kind: 'ok', content: 'ignored' } },
      })
    } finally {
      console.error = originalError
    }

    expect(warnings).toHaveLength(1)
    await Promise.resolve()
    expect(settled).toBe(false)
    expect(toolTasks()).toHaveLength(1)

    respond(taskId, { kind: 'ok', content: 'real result' })
    await expect(p).resolves.toBe('real result')
  })

  test('workflow/kill 以 cancelled 拒绝未决调用', async () => {
    capture()
    installToolGlobal('run-tool')
    const p = tool('Read', { file_path: '/a' })
    taskParams()

    await handleRequest({ jsonrpc: '2.0', id: 71, method: 'workflow/kill' })

    const error = await expectToolError(p)
    expect(error.code).toBe('cancelled')
    expect(error.tool).toBe('Read')
    expect(error.message).toBe('workflow killed by host')
  })

  test('run 结束收口以 cancelled 结算全部未决调用', async () => {
    const first = registerToolCall('tool-end-1', 'Read')
    const second = registerToolCall('tool-end-2', 'Bash')
    rejectPendingToolCalls('workflow run ended before the tool call settled')

    await expect(first).resolves.toEqual({
      kind: 'error',
      code: 'cancelled',
      tool: 'Read',
      message: 'workflow run ended before the tool call settled',
    })
    await expect(second).resolves.toEqual({
      kind: 'error',
      code: 'cancelled',
      tool: 'Bash',
      message: 'workflow run ended before the tool call settled',
    })
  })
})

// ─── 记录回放（issue #407 阶段 2）────────────────────────────

describe('tool() 记录回放', () => {
  test('命中记录：返回记录值、不发 tool/task、先发 tool/replayed', async () => {
    capture()
    installToolGlobal('run-tool')
    const entry = recorded({
      outcome: { kind: 'ok', content: 'text', structuredContent: { cached: true } },
    })
    configureReplay([entry])

    const p = tool('Read', { file_path: '/a' })

    // 回放通知在记录值交给脚本之前同步写出
    expect(toolTasks()).toHaveLength(0)
    expect(replayedNotifies()).toHaveLength(1)
    expect(replayedNotifies()[0]).toEqual({
      jsonrpc: '2.0',
      method: 'tool/replayed',
      params: { runId: 'run-tool', entry },
    })

    await expect(p).resolves.toEqual({ cached: true })
    expect(toolTasks()).toHaveLength(0)
  })

  test('记录无 structuredContent 时返回文本 content', async () => {
    capture()
    installToolGlobal('run-tool')
    configureReplay([recorded({ outcome: { kind: 'ok', content: 'text only' } })])

    await expect(tool('Read', { file_path: '/a' })).resolves.toBe('text only')
    expect(toolTasks()).toHaveLength(0)
    expect(replayedNotifies()).toHaveLength(1)
  })

  test('记录为 error：抛出带 code 的 WorkflowToolError', async () => {
    capture()
    installToolGlobal('run-tool')
    configureReplay([
      recorded({
        toolName: 'Bash',
        arguments: { command: 'rm -rf /tmp/x' },
        outcome: {
          kind: 'error',
          code: 'policy_denied',
          tool: 'Bash',
          message: 'blocked by policy',
          details: { rule: 'destructive' },
        },
      }),
    ])

    const error = await expectToolError(tool('Bash', { command: 'rm -rf /tmp/x' }))
    expect(error.code).toBe('policy_denied')
    expect(error.tool).toBe('Bash')
    expect(error.message).toBe('blocked by policy')
    expect(error.details).toEqual({ rule: 'destructive' })
    expect(toolTasks()).toHaveLength(0)
    expect(replayedNotifies()).toHaveLength(1)
  })

  test('arguments 深比较不等 → 转 live，live toolSeq 取当前 cursor', async () => {
    capture()
    installToolGlobal('run-tool')
    configureReplay([
      recorded({ arguments: { file_path: '/recorded' } }),
      recorded({ toolSeq: 1, arguments: { file_path: '/second' } }),
    ])

    const first = tool('Read', { file_path: '/different' })
    expect(taskParams(0).toolSeq).toBe(0)
    expect(taskParams(0).agentSeq).toBe(0)
    respond(taskParams(0).taskId, { kind: 'ok', content: 'live-1' })
    await expect(first).resolves.toBe('live-1')

    // 首个不匹配之后全部 live：即便第二条记录匹配也不再回放
    const second = tool('Read', { file_path: '/second' })
    expect(taskParams(1).toolSeq).toBe(1)
    respond(taskParams(1).taskId, { kind: 'ok', content: 'live-2' })
    await expect(second).resolves.toBe('live-2')

    expect(replayedNotifies()).toHaveLength(0)
  })

  test('普通对象键序无关仍可回放', async () => {
    capture()
    installToolGlobal('run-tool')
    configureReplay([
      recorded({ arguments: { first: 1, second: { nested: true } } }),
    ])

    await expect(
      tool('Read', { second: { nested: true }, first: 1 }),
    ).resolves.toBe('recorded')
    expect(replayedNotifies()).toHaveLength(1)
    expect(toolTasks()).toHaveLength(0)
  })

  test('非纯 JSON 嵌套对象（Date）不匹配空记录 → 转 live', async () => {
    capture()
    installToolGlobal('run-tool')
    configureReplay([recorded({ arguments: { value: {} } })])

    const p = tool('Read', { value: new Date(0) })
    expect(taskParams(0).toolSeq).toBe(0)
    respond(taskParams(0).taskId, { kind: 'ok', content: 'live' })
    await expect(p).resolves.toBe('live')

    expect(replayedNotifies()).toHaveLength(0)
  })

  test('entry.agentSeq 超出可回放前缀 → 不回放', async () => {
    capture()
    installToolGlobal('run-tool')
    configureReplay([recorded({ agentSeq: 2 })], 1)

    const p = tool('Read', { file_path: '/a' })
    expect(taskParams(0).toolSeq).toBe(0)
    respond(taskParams(0).taskId, { kind: 'ok', content: 'live' })
    await expect(p).resolves.toBe('live')

    expect(replayedNotifies()).toHaveLength(0)
  })

  test('toolSeq 不连续 → 该次起全部 live', async () => {
    capture()
    installToolGlobal('run-tool')
    configureReplay([
      recorded({ toolSeq: 0, arguments: { file_path: '/a' } }),
      recorded({ toolSeq: 5, arguments: { file_path: '/b' } }),
      recorded({ toolSeq: 2, arguments: { file_path: '/c' } }),
    ])

    await expect(tool('Read', { file_path: '/a' })).resolves.toBe('recorded')
    expect(toolTasks()).toHaveLength(0)

    const second = tool('Read', { file_path: '/b' })
    expect(taskParams(0).toolSeq).toBe(1)
    respond(taskParams(0).taskId, { kind: 'ok', content: 'live-b' })
    await expect(second).resolves.toBe('live-b')

    const third = tool('Read', { file_path: '/c' })
    expect(taskParams(1).toolSeq).toBe(2)
    respond(taskParams(1).taskId, { kind: 'ok', content: 'live-c' })
    await expect(third).resolves.toBe('live-c')

    expect(replayedNotifies()).toHaveLength(1)
  })

  test('journal 被 truncate 后不再回放，cursor 保持占位', async () => {
    capture()
    installToolGlobal('run-tool')
    configureReplay([
      recorded({ toolSeq: 0, arguments: { file_path: '/a' } }),
      recorded({ toolSeq: 1, arguments: { file_path: '/a' } }),
    ])

    await expect(tool('Read', { file_path: '/a' })).resolves.toBe('recorded')
    noteJournalReset()

    const second = tool('Read', { file_path: '/a' })
    expect(taskParams(0).toolSeq).toBe(1)
    respond(taskParams(0).taskId, { kind: 'ok', content: 'live' })
    await expect(second).resolves.toBe('live')

    expect(replayedNotifies()).toHaveLength(1)
  })

  test('agent_done 计数进入 live tool/task.agentSeq', async () => {
    capture()
    installToolGlobal('run-tool')
    configureReplay([])
    noteAgentCompleted()
    noteAgentCompleted()

    const p = tool('Read', { file_path: '/a' })
    expect(taskParams(0).agentSeq).toBe(2)
    expect(taskParams(0).toolSeq).toBe(0)
    respond(taskParams(0).taskId, { kind: 'ok', content: 'live' })
    await expect(p).resolves.toBe('live')
  })

  test('resetToolReplayState 清空回放与 agent 计数', async () => {
    capture()
    installToolGlobal('run-tool')
    configureReplay([recorded()], 3)
    noteAgentCompleted()
    resetToolReplayState()

    const p = tool('Read', { file_path: '/a' })
    expect(taskParams(0).toolSeq).toBe(0)
    expect(taskParams(0).agentSeq).toBe(0)
    respond(taskParams(0).taskId, { kind: 'ok', content: 'live' })
    await expect(p).resolves.toBe('live')
    expect(replayedNotifies()).toHaveLength(0)
  })
})

// ─── 全局安装 / 恢复 ───────────────────────────────────────

describe('installToolGlobal / uninstallToolGlobal', () => {
  test('安装可调用的 globalThis.tool，并恢复宿主旧值', async () => {
    capture()
    const target = globalThis as typeof globalThis & { tool?: unknown }
    const hostTool = () => 'host tool'
    target.tool = hostTool
    try {
      const restore = installToolGlobal('run-global')
      const installed = target.tool
      expect(typeof installed).toBe('function')
      expect(installed).not.toBe(hostTool)

      const p = (installed as (name: string, args?: Record<string, unknown>) => Promise<unknown>)(
        'Read',
        { file_path: '/a' },
      )
      respond(taskParams().taskId, { kind: 'ok', content: 'via global' })
      await expect(p).resolves.toBe('via global')

      restore()
      expect(target.tool).toBe(hostTool)
      restore() // 幂等
      expect(target.tool).toBe(hostTool)
    } finally {
      delete target.tool
    }
  })

  test('uninstallToolGlobal 恢复未安装前的缺省状态', () => {
    const target = globalThis as typeof globalThis & { tool?: unknown }
    expect(Object.hasOwn(target, 'tool')).toBe(false)

    installToolGlobal('run-global')
    expect(typeof target.tool).toBe('function')

    uninstallToolGlobal()
    expect(Object.hasOwn(target, 'tool')).toBe(false)
  })
})
