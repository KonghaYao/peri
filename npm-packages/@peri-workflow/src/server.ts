/**
 * JSON-RPC 服务端 — workflow/start、workflow/kill 分发与 workflow 执行。
 *
 * 生命周期：workflow/start → runWorkflowAsync（DAG 编排，agent 经 adapter 委托宿主）
 * → workflow/done 通知 → process.exit。
 */
import * as engine from '@claude-code-best/workflow-engine'
import type {
  AgentRunParams,
  AgentRunResult,
  JournalEntry,
  Logger,
  WorkflowPorts,
  WorkflowRunResult,
} from '@claude-code-best/workflow-engine'
import { rpcAdapter } from './adapter'
import { rejectPendingTasks, rejectPendingToolCalls, rpcNotify, send, waitDrain } from './rpc'
import {
  configureToolReplay,
  installToolGlobal,
  noteAgentCompleted,
  noteJournalReset,
  resetToolReplayState,
} from './tool'
import type { JsonRpcRequest, ToolJournalEntry, WorkflowJournalEntry, WorkflowStartParams } from './types'
import { WORKFLOW_BUILD_ID, WORKFLOW_PROTOCOL_VERSION } from './types'

let currentRunId: string
let currentAbortController: AbortController
let currentCwd: string
let currentBudget: number | null
let currentResumeRunId: string | undefined
let currentResumeJournal: WorkflowJournalEntry[] | undefined
let currentResumeTools: ToolJournalEntry[] | undefined

function parseBudgetTotal(params: unknown): number | undefined {
  if (!params || typeof params !== 'object' || !Object.hasOwn(params, 'budgetTotal')) {
    return undefined
  }
  const value = (params as { budgetTotal?: unknown }).budgetTotal
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`budgetTotal must be an integer between 1 and ${Number.MAX_SAFE_INTEGER}`)
  }
  return value
}

function parseResumeParams(params: unknown): string | undefined {
  if (!params || typeof params !== 'object') return undefined
  const raw = params as Record<string, unknown>
  const hasSource = Object.hasOwn(raw, 'resumeFromRunId')
  const hasJournal = Object.hasOwn(raw, 'resume')
  if (!hasSource && !hasJournal) return undefined
  // Rust's fresh-run wire shape may serialize an omitted resume source as
  // `resume: null`; preserve that legacy no-source form. Once a source run is
  // named, null/missing is a protocol error rather than a fresh execution.
  if (!hasSource) return raw.resume === null || Array.isArray(raw.resume) ? undefined : 'resume must be an array when provided'
  if (typeof raw.resumeFromRunId !== 'string' || raw.resumeFromRunId.length === 0) {
    return 'resumeFromRunId must be a non-empty string'
  }
  if (!hasJournal || !Array.isArray(raw.resume)) {
    return 'resume must be an array when resumeFromRunId is present'
  }
  return undefined
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return false
  const proto = Object.getPrototypeOf(value)
  return proto === Object.prototype || proto === null
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
}

/**
 * 校验一条 `resumeTools` 记录（fail closed：形状不全即拒绝整个 start）。
 *
 * 返回错误文本；`undefined` 表示合法。`outcome` 的载荷字段也一并校验，
 * 避免把半截记录写进 `tool/replayed` 后才在回放路径上失效。
 */
function validateResumeToolEntry(value: unknown, index: number): string | undefined {
  const at = `resumeTools[${index}]`
  if (!isPlainRecord(value)) return `${at} must be an object`
  if (!isNonNegativeSafeInteger(value.toolSeq)) {
    return `${at}.toolSeq must be a non-negative safe integer`
  }
  if (!isNonNegativeSafeInteger(value.agentSeq)) {
    return `${at}.agentSeq must be a non-negative safe integer`
  }
  if (typeof value.taskId !== 'string') return `${at}.taskId must be a string`
  if (typeof value.toolName !== 'string') return `${at}.toolName must be a string`
  if (!isPlainRecord(value.arguments)) return `${at}.arguments must be a plain object`

  const outcome = value.outcome
  if (!isPlainRecord(outcome)) return `${at}.outcome must be an object`
  if (outcome.kind === 'ok') {
    if (typeof outcome.content !== 'string') return `${at}.outcome.content must be a string`
    return undefined
  }
  if (outcome.kind === 'error') {
    if (typeof outcome.code !== 'string') return `${at}.outcome.code must be a string`
    if (typeof outcome.tool !== 'string') return `${at}.outcome.tool must be a string`
    if (typeof outcome.message !== 'string') return `${at}.outcome.message must be a string`
    return undefined
  }
  return `${at}.outcome.kind must be "ok" or "error"`
}

/**
 * 解析 `resumeTools`：省略 / null / 数组。数组元素必须通过
 * `validateResumeToolEntry`；任何非法形状都拒绝整个 start（-32602）。
 */
function parseResumeToolsParams(params: unknown): {
  entries?: ToolJournalEntry[]
  error?: string
} {
  if (!params || typeof params !== 'object') return {}
  const raw = params as Record<string, unknown>
  if (!Object.hasOwn(raw, 'resumeTools')) return {}
  const value = raw.resumeTools
  // 旧版本 fresh run 可能显式传 null；与省略同义。
  if (value === undefined || value === null) return {}
  if (!Array.isArray(value)) return { error: 'resumeTools must be an array when provided' }

  const entries: ToolJournalEntry[] = []
  for (const [index, item] of value.entries()) {
    const error = validateResumeToolEntry(item, index)
    if (error) return { error }
    entries.push(item as ToolJournalEntry)
  }
  return { entries }
}

function createPorts(): WorkflowPorts {
  return {
    agentAdapterRegistry: new engine.AgentAdapterRegistry()
      .register(rpcAdapter)
      .default('perihelion-rpc'),

    agentRunner: {
      async runAgentToResult(
        params: AgentRunParams,
        _host: unknown,
      ): Promise<AgentRunResult> {
        // fallback: should not be called when adapterRegistry is present
        return { kind: 'dead', reason: 'unknown', detail: 'agentRunner fallback — use adapterRegistry' }
      },
    },

    progressEmitter: {
      emit(event) {
        // agent_done 是本 run 已完成的 agent 数：脚本工具调用用它标 agentSeq。
        if (event.type === 'agent_done') noteAgentCompleted()
        rpcNotify('progress/event', event)
      },
    },

    taskRegistrar: {
      register() {
        return { runId: currentRunId, signal: currentAbortController.signal }
      },
      complete() {},
      fail() {},
      kill() {
        currentAbortController.abort()
      },
      pendingAction() {
        return null
      },
    },

    journalStore: {
      async read(): Promise<JournalEntry[]> {
        // A failed/skipped result is a cache boundary. Reusing entries after
        // it would let the engine return `null` for the failed call and would
        // make later results depend on an execution that never completed.
        const reusable: WorkflowJournalEntry[] = []
        let nextSeq = 0
        for (const entry of [...(currentResumeJournal ?? [])].sort((a, b) => a.seq - b.seq)) {
          if (entry.seq !== nextSeq || entry.result.kind !== 'ok') break
          nextSeq += 1
          reusable.push(entry)
        }
        // 回放前缀与 journal 可复用前缀同龄：只有引擎真正读取 journal
        // （续跑且脚本未变）后才启用回放；fresh run / scriptChanged 时
        // read 不会执行，回放保持禁用。
        configureToolReplay(currentResumeTools ?? [], reusable.length)
        return reusable.map((entry) => {
          const recovered: WorkflowJournalEntry = {
            ...entry,
            attempt: {
              runId: currentRunId,
              journalSeq: entry.seq,
              attemptIndex: entry.attempt?.attemptIndex,
              recoveredFrom: {
                runId: currentResumeRunId ?? currentRunId,
                agentId: entry.attempt?.agentId,
                journalSeq: entry.attempt?.journalSeq ?? entry.seq,
              },
              consumed: true,
              disposition: 'recovered' as const,
            },
          }
          rpcNotify('journal/append', { runId: currentRunId, entry: recovered })
          return recovered
        })
      },
      async append(runId: string, entry: JournalEntry): Promise<void> {
        // `attempt.execution`（执行结局、解析后的迭代上限与用量）由宿主在写入
        // journal 时按自己的执行事实补注：本进程看不到迭代计数与 run 预算，
        // 不在这里重复推导一份。
        const structured: WorkflowJournalEntry = {
          ...entry,
          attempt: {
            runId,
            journalSeq: entry.seq,
            consumed: true,
            disposition: 'produced',
          },
        }
        rpcNotify('journal/append', { runId, entry: structured })
      },
      async truncate(runId: string): Promise<void> {
        // 引擎判定 journal 分叉：关闭工具回放，但本 run 的 tools.jsonl 仍是
        // append-only（已回放条目继续占 toolSeq），cursor 不复位。
        noteJournalReset()
        rpcNotify('journal/truncate', { runId })
      },
    },

    permissionGate: { isAborted: () => false },

    logger: {
      debug(msg: string) {
        rpcNotify('log', { level: 'debug', message: msg })
      },
      event(name: string, meta?: Record<string, unknown>) {
        rpcNotify('log', { level: 'event', message: name, meta })
      },
      warn(msg: string) {
        rpcNotify('log', { level: 'warn', message: msg })
      },
      error(msg: string) {
        rpcNotify('log', { level: 'error', message: msg })
      },
    } as Logger & { error(msg: string): void },

    hostFactory(args?: { context?: unknown; canUseTool?: unknown; parentMessage?: unknown }) {
      return {
        handle: engine.createHostHandle(null),
        cwd: currentCwd,
        budgetTotal: currentBudget,
      }
    },
  }
}

export async function handleRequest(msg: JsonRpcRequest): Promise<void> {
  const { id, method, params } = msg

  switch (method) {
    case 'workflow/start': {
      const p = params as WorkflowStartParams
      let budgetTotal: number | undefined
      try {
        budgetTotal = parseBudgetTotal(params)
      } catch (error) {
        send({
          jsonrpc: '2.0',
          id: id!,
          error: {
            code: -32602,
            message: error instanceof Error ? error.message : 'invalid budgetTotal',
          },
        })
        return
      }
      const resumeError = parseResumeParams(params)
      if (resumeError) {
        send({
          jsonrpc: '2.0',
          id: id!,
          error: { code: -32602, message: resumeError },
        })
        return
      }
      const resumeTools = parseResumeToolsParams(params)
      if (resumeTools.error) {
        send({
          jsonrpc: '2.0',
          id: id!,
          error: { code: -32602, message: resumeTools.error },
        })
        return
      }
      currentRunId = p.runId
      currentCwd = p.cwd
      currentBudget = budgetTotal ?? null
      currentResumeRunId = p.resumeFromRunId
      currentResumeJournal = p.resume as WorkflowJournalEntry[] | undefined
      currentResumeTools = resumeTools.entries
      // 新 run 的脚本工具序号从 0 起；在途 pending 由 run 结束/kill 的既有
      // rejectPendingToolCalls 收口，不在这里清空。
      resetToolReplayState()
      currentAbortController = new AbortController()
      send({
        jsonrpc: '2.0',
        id: id!,
        result: {
          ok: true,
          protocolVersion: WORKFLOW_PROTOCOL_VERSION,
          buildId: WORKFLOW_BUILD_ID,
        },
      })

      runWorkflowAsync(p).catch(async (err: unknown) => {
        await waitDrain()
        rpcNotify('workflow/done', {
          runId: p.runId,
          status: 'failed' as const,
          error: String(err),
        })
        await waitDrain()
        process.exit(1)
      })
      return
    }

    case 'workflow/kill': {
      currentAbortController?.abort()
      // 未完成任务的等待项以取消编码就地结算：不残留悬挂 promise（design §5.3）。
      // 宿主侧 kill 也会取消任务并投递取消结果；此处兜底覆盖投递丢失的窗口。
      rejectPendingTasks('workflow killed by host')
      // 脚本工具调用不是 agent 任务：独立等待表同样以 `cancelled` 收口。
      rejectPendingToolCalls('workflow killed by host')
      send({
        jsonrpc: '2.0',
        id: id!,
        result: { ok: true },
      })
      return
    }

    default:
      send({
        jsonrpc: '2.0',
        id: id!,
        error: {
          code: -32601,
          message: `unknown method: ${method}`,
        },
      })
  }
}

async function runWorkflowAsync({
  runId,
  script,
  args,
  maxConcurrency,
}: WorkflowStartParams): Promise<void> {
  engine.parseScript(script) // validate early

  // 脚本工具原语：run 期间把 `globalThis.tool` 指向宿主工具通道；嵌套
  // `workflow()` 复用同一 run/hook，run 结束或 kill 后恢复宿主原有全局值。
  const restoreToolGlobal = installToolGlobal(runId)
  let result: WorkflowRunResult
  try {
    result = await engine.runWorkflow({
      script,
      args,
      runId,
      ports: createPorts(),
      host: engine.createHostHandle(null),
      signal: currentAbortController.signal,
      cwd: currentCwd,
      budgetTotal: currentBudget,
      maxConcurrency,
      resume: !!currentResumeJournal,
    })
  } finally {
    restoreToolGlobal()
    // 引擎退出后仍未结算的工具调用不会再有消费者：以 `cancelled` 收口，
    // 不残留悬挂 promise（宿主投递丢失或 kill 窗口）。
    rejectPendingToolCalls('workflow run ended before the tool call settled')
  }

  // 排空 stdout 后再发送终态通知
  await waitDrain()

  rpcNotify('workflow/done', {
    runId,
    status: result.status,
    returnValue: result.returnValue,
    error: result.error,
  })

  // 确保 workflow/done 消息被写入后再退出
  await waitDrain()

  process.exit(0)
}
