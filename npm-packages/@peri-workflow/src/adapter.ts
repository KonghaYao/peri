/**
 * AgentAdapter — 把 engine 的 agent() 调用委托给宿主执行。
 *
 * P6 wire：发布 `agent/task` 通知（宿主登记任务 → 宿主消费者执行），等待宿主
 * `agent/result` 通知结算（taskId 去重）。宿主拥有模型选择、API 密钥、工具与
 * 安全边界；取消编码为 `dead { reason: 'killed' }` → 按引擎约定抛
 * `WorkflowAbortedError`（与旧链 `agent/run` 的 `-32000` 错误响应同语义）。
 */
import * as engine from '@claude-code-best/workflow-engine'
import type {
  AgentAdapter,
  AgentAdapterContext,
  AgentRunParams,
  AgentRunResult,
} from '@claude-code-best/workflow-engine'
import { registerTask, rpcNotify } from './rpc'
import type { AgentTaskParams } from './types'

/** taskId 序号：作用域 = 单次 run（runId 前缀保证跨 run 不复用）。 */
let taskSeq = 0

/**
 * 宿主取消编码（P6 wire）：`dead { reason: 'killed' }`。
 *
 * 该编码由本仓 wire 约定定义；引擎 0.1.0 的 `AgentRunResult.reason` 联合尚未包含
 * 它，因此按 `unknown` 比较，避免与引擎类型声明产生假冲突。
 */
function isCancelledResult(result: AgentRunResult | undefined): boolean {
  if (!result || result.kind !== 'dead') return false
  return (result.reason as unknown) === 'killed'
}

export const rpcAdapter: AgentAdapter = {
  id: 'perihelion-rpc',
  capabilities: { structuredOutput: true, tools: true },

  async run(params: AgentRunParams, ctx: AgentAdapterContext): Promise<AgentRunResult> {
    const taskId = `${ctx.runId}:${++taskSeq}`
    const pending = registerTask(taskId)
    rpcNotify('agent/task', {
      taskId,
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
    } satisfies AgentTaskParams)

    const result = (await pending) as AgentRunResult | undefined
    if (isCancelledResult(result)) {
      throw new engine.WorkflowAbortedError()
    }
    if (!result) {
      return { kind: 'dead', reason: 'runagent-threw', detail: 'empty agent/result payload' }
    }
    return result
  },
}
