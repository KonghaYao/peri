/**
 * AgentAdapter — 把 engine 的 agent() 调用委托给宿主执行。
 *
 * 通过 JSON-RPC `agent/run` 请求宿主（Rust/Go/Python）跑 LLM ReAct 循环，
 * 宿主拥有模型选择、API 密钥、工具与安全边界。
 */
import * as engine from '@claude-code-best/workflow-engine'
import type {
  AgentAdapter,
  AgentAdapterContext,
  AgentRunParams,
  AgentRunResult,
} from '@claude-code-best/workflow-engine'
import { rpcRequest } from './rpc'
import type { AgentRunRequestParams } from './types'

export const rpcAdapter: AgentAdapter = {
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
