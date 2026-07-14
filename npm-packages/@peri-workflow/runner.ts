#!/usr/bin/env node
/**
 * @peri-code/workflow — JSON-RPC 2.0 stdio bridge to the workflow engine.
 *
 * ## Architecture
 *
 *  ┌─────────────┐  JSON-RPC 2.0   ┌──────────────────────┐
 *  │  Host (Rust, │ ◄─────────────► │  @peri-code/workflow      │
 *  │  Go, Python) │  stdin/stdout   │  (this process)      │
 *  └─────────────┘                  │  ┌────────────────┐  │
 *                                   │  │ workflow-engine │  │
 *                                   │  └────────────────┘  │
 *                                   └──────────────────────┘
 *
 * The host owns the UI, session management, LLM API keys, and agent execution.
 * This process owns the workflow DAG execution (agent(), parallel(), pipeline(), phase()).
 * The host implements an agent backend that responds to `agent/run` RPC requests.
 *
 * ## Protocol (host → runner requests)
 *   workflow/start — begin a new workflow run
 *   workflow/kill  — abort the running workflow
 *
 * ## Protocol (runner → host requests)
 *   agent/run — execute one agent call (host must respond)
 *
 * ## Protocol (runner → host notifications)
 *   progress/event    — progress updates
 *   journal/append    — persist a journal entry
 *   journal/truncate  — truncate the journal
 *   log               — logging messages
 *   workflow/done     — workflow completed (terminal)
 */

import * as engine from '@claude-code-best/workflow-engine'
import * as readline from 'readline'
import type {
  AgentAdapter,
  AgentAdapterContext,
  AgentAdapterRegistry,
} from '@claude-code-best/workflow-engine'
import type {
  AgentRunParams,
  AgentRunResult,
  JournalEntry,
  Logger,
  WorkflowPorts,
  WorkflowRunResult,
} from '@claude-code-best/workflow-engine'

// ═══════════════════════════════════════════════════════════
//  JSON-RPC 2.0 wire types
// ═══════════════════════════════════════════════════════════

type JsonRpcId = string | number

type JsonRpcRequest = {
  jsonrpc: '2.0'
  id?: JsonRpcId
  method: string
  params?: unknown
}

type JsonRpcResponse = {
  jsonrpc: '2.0'
  id: JsonRpcId
  result?: unknown
  error?: { code: number; message: string; data?: unknown }
}

type JsonRpcNotification = {
  jsonrpc: '2.0'
  method: string
  params?: unknown
}

type JsonRpcMessage = JsonRpcRequest | JsonRpcResponse | JsonRpcNotification

// ═══════════════════════════════════════════════════════════
//  RPC method signatures
// ═══════════════════════════════════════════════════════════

/** host → runner: start a workflow */
type WorkflowStartParams = {
  runId: string
  cwd: string
  budgetTotal: number | null
  resume?: JournalEntry[]
  script: string
  args?: unknown
  maxConcurrency?: number
}

/** runner → host: execute one agent call */
type AgentRunRequestParams = AgentRunParams & {
  runId: string
  agentId: number
}

// ═══════════════════════════════════════════════════════════
//  RPC transport
// ═══════════════════════════════════════════════════════════

const rl = readline.createInterface({ input: process.stdin })

let reqId = 100
const pending = new Map<
  JsonRpcId,
  { resolve: (value: unknown) => void; reject: (error: unknown) => void }
>()

function send(msg: JsonRpcMessage): void {
  process.stdout.write(JSON.stringify(msg) + '\n')
}

function rpcRequest(method: string, params: unknown): Promise<unknown> {
  const id = reqId++
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject })
    send({ jsonrpc: '2.0', id, method, params })
  })
}

function rpcNotify(method: string, params: unknown): void {
  send({ jsonrpc: '2.0', method, params })
}

function handleResponse(msg: JsonRpcResponse): void {
  const entry = pending.get(msg.id)
  if (!entry) return
  pending.delete(msg.id)
  if (msg.error) {
    entry.reject(msg.error)
  } else {
    entry.resolve(msg.result)
  }
}

// ═══════════════════════════════════════════════════════════
//  AgentAdapter — delegates agent() calls to the host via RPC
// ═══════════════════════════════════════════════════════════

const rpcAdapter: AgentAdapter = {
  id: 'perihelion-rpc',
  capabilities: { structuredOutput: true, tools: true },

  async run(params: AgentRunParams, ctx: AgentAdapterContext): Promise<AgentRunResult> {
    try {
      return (await rpcRequest('agent/run', {
        runId: ctx.runId,
        agentId: ctx.agentId,
        prompt: params.prompt,
        schema: params.schema,
        model: params.model,
        maxTokens: params.maxTokens,
        agentType: params.agentType,
        isolation: params.isolation,
        allowedTools: params.allowedTools,
        label: params.label,
        phase: params.phase,
      } satisfies AgentRunRequestParams)) as AgentRunResult
    } catch (err: unknown) {
      if (
        typeof err === 'object' &&
        err !== null &&
        'code' in err &&
        (err as { code: number }).code === -32000
      ) {
        throw new engine.WorkflowAbortedError()
      }
      return { kind: 'dead', reason: 'runagent-threw', detail: String(err) }
    }
  },
}

// ═══════════════════════════════════════════════════════════
//  WorkflowPorts factory — wires engine callbacks to RPC
// ═══════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════
//  Request dispatcher
// ═══════════════════════════════════════════════════════════

async function handleRequest(msg: JsonRpcRequest): Promise<void> {
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

      runWorkflowAsync(p).catch((err: unknown) => {
        rpcNotify('workflow/done', {
          runId: p.runId,
          status: 'failed' as const,
          error: String(err),
        })
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

// ═══════════════════════════════════════════════════════════
//  Workflow execution
// ═══════════════════════════════════════════════════════════

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

  rpcNotify('workflow/done', {
    runId,
    status: result.status,
    returnValue: result.returnValue,
    error: result.error,
  })
  process.exit(0)
}

// ═══════════════════════════════════════════════════════════
//  Main loop
// ═══════════════════════════════════════════════════════════

rl.on('line', (line: string) => {
  let msg: JsonRpcMessage
  try {
    msg = JSON.parse(line)
  } catch {
    return // ignore invalid JSON lines
  }

  // Is this a response to a pending request?
  if (
    'id' in msg &&
    msg.id !== undefined &&
    ('result' in msg || 'error' in msg)
  ) {
    handleResponse(msg as JsonRpcResponse)
    return
  }

  // Is this an incoming request?
  if ('method' in msg && msg.method) {
    void handleRequest(msg as JsonRpcRequest)
  }
})
