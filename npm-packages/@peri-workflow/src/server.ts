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
import { rpcNotify, send, waitDrain } from './rpc'
import type { JsonRpcRequest, WorkflowJournalEntry, WorkflowStartParams } from './types'
import { WORKFLOW_BUILD_ID, WORKFLOW_PROTOCOL_VERSION } from './types'

let currentRunId: string
let currentAbortController: AbortController
let currentCwd: string
let currentBudget: number | null
let currentResumeRunId: string | undefined
let currentResumeJournal: WorkflowJournalEntry[] | undefined

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
        return (currentResumeJournal ?? []).map((entry) => {
          const recovered: WorkflowJournalEntry = {
            ...entry,
            attempt: {
              runId: currentRunId,
              journalSeq: entry.seq,
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
      currentRunId = p.runId
      currentCwd = p.cwd
      currentBudget = budgetTotal ?? null
      currentResumeRunId = p.resumeFromRunId
      currentResumeJournal = p.resume as WorkflowJournalEntry[] | undefined
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

  const result: WorkflowRunResult = await engine.runWorkflow({
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
