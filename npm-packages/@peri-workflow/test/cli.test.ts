/**
 * CLI 分发测试 — cliMain 的 read/list/help 成功路径与错误路径。
 *
 * 构造临时 `.claude/workflow-runs/` 目录并 chdir 进去；捕获 console 输出，
 * process.exit 置为 noop（错误路径不杀测试进程）。
 */
import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { cliMain, isCliCommand } from '../src/cli'
import { listRuns } from '../src/reader'

// ─── 进程级 patch + 临时环境 ───────────────────────────────

const origExit = process.exit
const origLog = console.log
const origError = console.error
const origCwd = process.cwd()

let logs: string[] = []
let errs: string[] = []
let runsRoot: string

/** 错误路径的真实行为是 process.exit；测试里改为 throw 以便断言且不杀进程 */
process.exit = ((code?: number) => {
  throw new Error(`process.exit(${code ?? 0})`)
}) as typeof process.exit
console.log = ((...a: unknown[]) => {
  logs.push(a.join(' '))
}) as typeof console.log
console.error = ((...a: unknown[]) => {
  errs.push(a.join(' '))
}) as typeof console.error

beforeAll(() => {
  const base = mkdtempSync(join(tmpdir(), 'workflow-cli-'))
  runsRoot = join(base, '.claude', 'workflow-runs')
  const runDir = join(runsRoot, 'run-1')
  mkdirSync(join(runDir, 'outputs'), { recursive: true })
  writeFileSync(
    join(runDir, 'state.json'),
    JSON.stringify({
      run_id: 'run-1',
      workflow_name: 'demo',
      status: 'completed',
      return_value: { summary: '${fix}', list: ['a', '${fix}'] },
      script: '',
      started_at: '2026-08-02T00:00:00Z',
      finished_at: '2026-08-02T00:01:30Z',
    })
  )
  writeFileSync(join(runDir, 'outputs', 'fix.txt'), '长文本内容')
  writeFileSync(
    join(runDir, 'journal.jsonl'),
    JSON.stringify({
      key: 'k1',
      seq: 1,
      result: { kind: 'ok', output: 'agent 输出', usage: { outputTokens: 42 }, durationMs: 1500 },
    }) + '\n'
  )
  // validate 子命令的 fixture 脚本
  writeFileSync(
    join(base, 'good.mjs'),
    `export const meta = { name: 'demo', description: 'ok' }\nconst r = await agent('hi')\nreturn r`
  )
  writeFileSync(
    join(base, 'bad.mjs'),
    `export const meta = { name: 'demo', description: 'ok' }\nconst r = await workflow.agent('hi')\nreturn r`
  )
  process.chdir(base)
})

afterAll(() => {
  process.exit = origExit
  console.log = origLog
  console.error = origError
  process.chdir(origCwd)
})

// ─── isCliCommand ──────────────────────────────────────────

describe('isCliCommand', () => {
  test('read/list/validate/help 变体都是 CLI 命令', () => {
    expect(isCliCommand('read')).toBe(true)
    expect(isCliCommand('list')).toBe(true)
    expect(isCliCommand('validate')).toBe(true)
    expect(isCliCommand('--help')).toBe(true)
    expect(isCliCommand('-h')).toBe(true)
    expect(isCliCommand('help')).toBe(true)
  })

  test('无参数/未知参数不是 CLI 命令', () => {
    expect(isCliCommand(undefined)).toBe(false)
    expect(isCliCommand('')).toBe(false)
    expect(isCliCommand('workflow/start')).toBe(false)
    expect(isCliCommand('foo')).toBe(false)
  })
})

// ─── cliMain read ──────────────────────────────────────────

describe('cliMain read', () => {
  test('--json：输出结构化 JSON，占位符原位替换', () => {
    logs = []
    cliMain(['read', 'run-1', '--json'])
    const result = JSON.parse(logs.join('\n'))
    expect(result.run_id).toBe('run-1')
    expect(result.status).toBe('completed')
    expect(result.duration).toBe('1m30s')
    expect(result.return_value).toEqual({ summary: '长文本内容', list: ['a', '长文本内容'] })
    expect(result.agents).toHaveLength(1)
    expect(result.agents[0].tokens).toBe(42)
  })

  test('--short：状态 + agent 统计表', () => {
    logs = []
    cliMain(['read', 'run-1', '--short'])
    const out = logs.join('\n')
    expect(out).toContain('# Workflow Run run-1 — demo')
    expect(out).toContain('status: completed')
    expect(out).toContain('| # | phase | status | tokens | tools | 耗时 | 摘要 |')
    expect(out).toContain('| 1 | - | ok | 42 | - | 1.5s |')
  })

  test('完整模式：含 return value 与 agent 全量输出', () => {
    logs = []
    cliMain(['read', 'run-1'])
    const out = logs.join('\n')
    expect(out).toContain('## Return value')
    expect(out).toContain('### summary')
    expect(out).toContain('长文本内容')
    expect(out).toContain('--- Agent 1')
  })

  test('无 runId：报用法并退出', () => {
    logs = []
    errs = []
    expect(() => cliMain(['read'])).toThrow('process.exit(1)')
    expect(errs.join('\n')).toContain('用法：peri-workflow read <runId>')
  })

  test('非法 runId（路径遍历）：拒绝', () => {
    logs = []
    errs = []
    expect(() => cliMain(['read', '../evil'])).toThrow('process.exit(1)')
    expect(errs.join('\n')).toContain('非法 runId')
  })

  test('不存在的 run：提示可 list', () => {
    logs = []
    errs = []
    expect(() => cliMain(['read', 'no-such-run'])).toThrow('process.exit(1)')
    expect(errs.join('\n')).toContain('未找到运行 no-such-run')
  })
})

// ─── cliMain validate ───────────────────────────────────────

describe('cliMain validate', () => {
  test('合法脚本 --json：ok + meta + exit 0', () => {
    logs = []
    cliMain(['validate', 'good.mjs', '--json'])
    const r = JSON.parse(logs.join('\n'))
    expect(r.ok).toBe(true)
    expect(r.meta.name).toBe('demo')
    expect(r.errors).toEqual([])
  })

  test('坏脚本 --json：errors 含 workflow. 旧式调用提示', () => {
    logs = []
    expect(() => cliMain(['validate', 'bad.mjs', '--json'])).toThrow('process.exit(1)')
    const r = JSON.parse(logs.join('\n'))
    expect(r.ok).toBe(false)
    expect(r.errors[0]).toContain('workflow.agent(')
  })

  test('合法脚本文本模式：✓ 校验通过', () => {
    logs = []
    cliMain(['validate', 'good.mjs'])
    expect(logs.join('\n')).toContain('✓ good.mjs 校验通过 (demo)')
  })

  test('坏脚本文本模式：✗ 校验失败列出错误', () => {
    logs = []
    expect(() => cliMain(['validate', 'bad.mjs'])).toThrow('process.exit(1)')
    const out = logs.join('\n')
    expect(out).toContain('✗ bad.mjs 校验失败（1 个错误）')
    expect(out).toContain('workflow.agent(')
  })

  test('无文件参数：报用法并退出', () => {
    logs = []
    errs = []
    expect(() => cliMain(['validate'])).toThrow('process.exit(1)')
    expect(errs.join('\n')).toContain('用法：peri-workflow validate <script.mjs>')
  })

  test('文件不存在：exit 1 + 提示', () => {
    logs = []
    errs = []
    expect(() => cliMain(['validate', 'no-such.mjs'])).toThrow('process.exit(1)')
    expect(errs.join('\n')).toContain('无法读取文件: no-such.mjs')
  })
})

// ─── cliMain list / help ───────────────────────────────────

describe('cliMain list / help', () => {
  test('list：文本表格含 run-1', () => {
    logs = []
    cliMain(['list'])
    const out = logs.join('\n')
    expect(out).toContain('# Workflow runs (1)')
    expect(out).toContain('| run_id | workflow | status | 时长 | finished_at |')
    expect(out).toContain('| run-1 | demo | completed | 1m30s |')
  })

  test('list --json：JSON 数组', () => {
    logs = []
    cliMain(['list', '--json'])
    const runs = JSON.parse(logs.join('\n'))
    expect(runs).toHaveLength(1)
    expect(runs[0].run_id).toBe('run-1')
  })

  test('--help：输出用法', () => {
    logs = []
    expect(() => cliMain(['--help'])).toThrow('process.exit(0)')
    expect(logs.join('\n')).toContain('用法（CLI 子命令）')
  })
})

// ─── reader 直接调用（无 runs 根的错误路径）───────────────

describe('reader 错误路径', () => {
  test('findRunsRoot 找不到时 listRuns 报错', () => {
    const empty = mkdtempSync(join(tmpdir(), 'workflow-empty-'))
    process.chdir(empty)
    logs = []
    errs = []
    expect(() => listRuns(false)).toThrow('process.exit(1)')
    expect(errs.join('\n')).toContain('未找到 .claude/workflow-runs 目录')
    process.chdir(runsRoot.slice(0, -'/workflow-runs'.length))
  })
})
