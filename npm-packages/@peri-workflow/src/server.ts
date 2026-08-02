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
import type { JsonRpcRequest, WorkflowStartParams } from './types'

let currentRunId: string
let currentAbortController: AbortController
let currentCwd: string
let currentBudget: number | null
let currentResumeJournal: JournalEntry[] | undefined

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
        return currentResumeJournal ?? []
      },
      async append(runId: string, entry: JournalEntry): Promise<void> {
        rpcNotify('journal/append', { runId, entry })
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
      currentRunId = p.runId
      currentCwd = p.cwd
      currentBudget = p.budgetTotal
      currentResumeJournal = p.resume
      currentAbortController = new AbortController()
      send({
        jsonrpc: '2.0',
        id: id!,
        result: { ok: true },
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
