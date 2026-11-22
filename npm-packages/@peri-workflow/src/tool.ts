/**
 * `tool()` 脚本原语 — 把脚本内的确定性 MCP 工具调用委托给宿主执行
 * （issue #407 阶段 1），并在续跑时按宿主记录前缀确定性回放（阶段 2）。
 *
 * 脚本面：
 *
 * ```js
 * const result = await tool('Read', { file_path: '/abs/path' })
 * ```
 *
 * live 路径：本进程发布 `tool/task` 通知、等待宿主 `tool/result` 通知，再把结果
 * 返回脚本。宿主负责 effective 名解析、审批与真实执行；本进程只做对象形状检查，
 * 不复刻 MCP schema 校验，也不构成安全边界（脚本仍可使用 Node 原生能力）。
 *
 * 回放路径：宿主经 `workflow/start.resumeTools` 传回本 run 的 `tools.jsonl` 前缀。
 * 只有 `journalStore.read()` 真正执行（续跑且脚本未变）后才启用回放；匹配项直接
 * 返回记录值并发送 `tool/replayed`，不发布 `tool/task`。首个不匹配起全部转 live。
 *
 * 生命周期：run 开始时 `installToolGlobal(runId)`，run 结束时调用其返回的
 * restore（或 `uninstallToolGlobal()`）恢复旧值。宿主 stop/kill 由
 * `rejectPendingToolCalls` 以 `cancelled` 结算未决调用。
 */
import { registerToolCall, rpcNotify } from './rpc'
import type {
  ToolJournalEntry,
  ToolReplayedParams,
  ToolTaskParams,
  WorkflowToolErrorCode,
  WorkflowToolOutcome,
} from './types'

/** 脚本侧工具调用错误：保留宿主给出的 code / tool / message / details。 */
export class WorkflowToolError extends Error {
  readonly code: WorkflowToolErrorCode
  readonly tool: string
  readonly details?: unknown

  constructor(
    code: WorkflowToolErrorCode,
    tool: string,
    message: string,
    details?: unknown,
  ) {
    super(message)
    this.name = 'WorkflowToolError'
    this.code = code
    this.tool = tool
    this.details = details
  }
}

type ToolHook = (
  effectiveName: string,
  args?: Record<string, unknown>,
) => Promise<unknown>

type GlobalWithTool = typeof globalThis & { tool?: unknown }

/** taskId 序号：单进程只执行一个 run，序号仍保证 run 内（及跨测试安装）唯一。 */
let toolTaskSeq = 0

/** 当前已安装 hook 的 runId；未安装时 `tool()` 以 `tool_unavailable` 失败。 */
let activeRunId: string | undefined

/** 最近一次安装的恢复函数（`uninstallToolGlobal` 用）。 */
let restoreInstalled: (() => void) | undefined

function isPlainObject(value: unknown): value is Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    return false
  }
  const proto = Object.getPrototypeOf(value)
  return proto === Object.prototype || proto === null
}

function isToolOutcome(value: unknown): value is WorkflowToolOutcome {
  if (value === null || typeof value !== 'object') return false
  const kind = (value as { kind?: unknown }).kind
  return kind === 'ok' || kind === 'error'
}

function errorToolName(effectiveName: unknown): string {
  return typeof effectiveName === 'string' ? effectiveName : String(effectiveName)
}

// ─── 工具调用回放状态（issue #407 阶段 2）────────────────────
//
// 单进程只执行一个 run：模块级状态即可，`workflow/start` 时整体复位。
// `cursor` 是 run 内脚本工具调用序号（回放与 live 共用）；本 run 的
// `tools.jsonl` 是 append-only，已回放条目继续占位，因此 journal 被 engine
// truncate 时不重置 `cursor`，只关闭回放。

let replayEntries: ToolJournalEntry[] = []
let replayCursor = 0
let replayEnabled = false
/** `journalStore.read()` 时可复用的 agent 前缀长度（已完成的 agent 数）。 */
let replayedAgentPrefix = 0
/** 本 run 已观察到的 `agent_done` 事件数；live `tool/task.agentSeq` 取自它。 */
let completedAgents = 0

/** 清空全部回放/计数状态（`workflow/start` 时调用；不触碰在途 pending）。 */
export function resetToolReplayState(): void {
  replayEntries = []
  replayCursor = 0
  replayEnabled = false
  replayedAgentPrefix = 0
  completedAgents = 0
}

/**
 * 配置本次 run 的工具记录回放。
 *
 * 只在 `journalStore.read()` 真正执行后调用：fresh run / scriptChanged 时引擎
 * 不读 journal，回放必须保持禁用。
 */
export function configureToolReplay(
  entries: ToolJournalEntry[],
  replayedAgentPrefixCount: number,
): void {
  replayEntries = [...entries]
  replayCursor = 0
  replayEnabled = entries.length > 0
  replayedAgentPrefix = replayedAgentPrefixCount
}

/** 记录一次已完成的 `agent_done`（含 journal 命中的 agent 调用）。 */
export function noteAgentCompleted(): void {
  completedAgents += 1
}

/**
 * journal 被 engine 判定分叉（truncate）时调用：关闭回放。
 *
 * `cursor` 保持不变——本 run 已回放的工具调用仍在 append-only 的
 * `tools.jsonl` 中占位。
 */
export function noteJournalReset(): void {
  replayEnabled = false
}

/**
 * 只把纯 JSON 对象视为记录：`Date` / `Map` / `Set` / 类实例即使没有自有可枚举
 * 键，也不能与记录中的 `{}` 匹配（wire 上它们会变成字符串或丢失，fail closed）。
 */
function isPlainJsonObject(value: object): boolean {
  const prototype = Object.getPrototypeOf(value)
  return prototype === Object.prototype || prototype === null
}

/**
 * JSON 深度相等（arguments 回放匹配用）。
 *
 * 只处理 JSON 值域：对象键序无关；数组按位置比较；`undefined` 与缺省不等价
 * （脚本传 `{ a: undefined }` 时 wire 会丢键，此时按不匹配转 live，fail closed）。
 */
function jsonDeepEqual(left: unknown, right: unknown): boolean {
  if (left === right) return true
  if (left === null || right === null) return false
  if (typeof left !== 'object' || typeof right !== 'object') return false
  if (Array.isArray(left) || Array.isArray(right)) {
    if (!Array.isArray(left) || !Array.isArray(right)) return false
    if (left.length !== right.length) return false
    return left.every((item, index) => jsonDeepEqual(item, right[index]))
  }
  if (!isPlainJsonObject(left) || !isPlainJsonObject(right)) return false
  const leftRecord = left as Record<string, unknown>
  const rightRecord = right as Record<string, unknown>
  const leftKeys = Object.keys(leftRecord)
  const rightKeys = Object.keys(rightRecord)
  if (leftKeys.length !== rightKeys.length) return false
  return leftKeys.every(
    (key) =>
      Object.hasOwn(rightRecord, key) &&
      jsonDeepEqual(leftRecord[key], rightRecord[key]),
  )
}

/** 该记录是否与脚本本次调用逐字段匹配（不检查 cursor 位置）。 */
function matchesReplayEntry(
  entry: ToolJournalEntry,
  scriptToolSeq: number,
  effectiveName: string,
  args: Record<string, unknown> | undefined,
): boolean {
  return (
    entry.toolSeq === scriptToolSeq &&
    entry.agentSeq <= replayedAgentPrefix &&
    entry.toolName === effectiveName &&
    jsonDeepEqual(entry.arguments, args ?? {})
  )
}

/**
 * 回放一条记录：先发 `tool/replayed`（宿主补记 `tools.jsonl`），再按记录值
 * 返回或抛错。不发布 `tool/task`。
 */
function replayRecordedToolCall(runId: string, entry: ToolJournalEntry): unknown {
  rpcNotify('tool/replayed', { runId, entry } satisfies ToolReplayedParams)
  if (entry.outcome.kind === 'error') {
    throw new WorkflowToolError(
      entry.outcome.code,
      entry.outcome.tool,
      entry.outcome.message,
      entry.outcome.details,
    )
  }
  return entry.outcome.structuredContent ?? entry.outcome.content
}

/**
 * 发布一次脚本工具调用并等待宿主终态。
 *
 * `toolSeq` 是脚本调用序号（与 taskId 序号相互独立），`agentSeq` 是调用前
 * 已完成的 `agent()` 数。失败时抛 `WorkflowToolError`（脚本可 `try/catch`）；
 * `structuredContent` 非 null 时优先返回，否则返回文本 `content`。结果不截断、
 * 不重校验 schema。
 */
async function dispatchToolCall(
  runId: string,
  effectiveName: string,
  args: Record<string, unknown> | undefined,
  toolSeq: number,
  agentSeq: number,
): Promise<unknown> {
  const taskId = `tool-${++toolTaskSeq}`
  const pending = registerToolCall(taskId, effectiveName)
  rpcNotify('tool/task', {
    taskId,
    runId,
    toolName: effectiveName,
    arguments: args ?? {},
    toolSeq,
    agentSeq,
  } satisfies ToolTaskParams)

  const raw: unknown = await pending
  if (!isToolOutcome(raw)) {
    throw new WorkflowToolError(
      'protocol_error',
      effectiveName,
      'malformed tool/result outcome',
    )
  }
  if (raw.kind === 'error') {
    throw new WorkflowToolError(raw.code, raw.tool, raw.message, raw.details)
  }
  // Rust 侧 `Option<Value>`：`null` 与缺省同义，按文本兜底。
  return raw.structuredContent ?? raw.content
}

/**
 * 脚本可见的 `tool()`：只接受 effective 名与普通对象参数。
 *
 * 本地形状检查先于任务发布/回放：非法名 / 参数不会产生 `tool/task`。
 * 本 run 首个不匹配的记录起全部转 live（此后不再尝试回放）。
 */
export async function tool(
  effectiveName: string,
  args?: Record<string, unknown>,
): Promise<unknown> {
  if (typeof effectiveName !== 'string' || effectiveName.trim().length === 0) {
    throw new WorkflowToolError(
      'invalid_arguments',
      errorToolName(effectiveName),
      'tool name must be a non-empty string',
    )
  }
  if (args !== undefined && !isPlainObject(args)) {
    throw new WorkflowToolError(
      'invalid_arguments',
      effectiveName,
      'tool arguments must be a plain object',
    )
  }

  const runId = activeRunId
  if (!runId) {
    throw new WorkflowToolError(
      'tool_unavailable',
      effectiveName,
      'tool() is not available outside an active workflow run',
    )
  }

  const seq = replayCursor
  if (replayEnabled && seq < replayEntries.length) {
    const entry = replayEntries[seq]!
    if (matchesReplayEntry(entry, seq, effectiveName, args)) {
      replayCursor += 1
      return replayRecordedToolCall(runId, entry)
    }
  }

  // 首个不匹配（或回放未启用）：本 run 余下调用全部 live；序号继续占位。
  replayEnabled = false
  replayCursor += 1
  return dispatchToolCall(runId, effectiveName, args, seq, completedAgents)
}

/**
 * 安装 `globalThis.tool` 到当前 run；返回恢复函数（幂等）。
 *
 * 嵌套 `workflow()` 复用同一进程 / runId，因此只安装一次即可。重复安装会先
 * 恢复上一次，避免 hook 堆叠丢失宿主原有全局值。
 */
export function installToolGlobal(runId: string): () => void {
  restoreInstalled?.()

  const target = globalThis as GlobalWithTool
  const hadPrevious = Object.hasOwn(target, 'tool')
  const previous = target.tool
  const hook: ToolHook = (effectiveName, args) => tool(effectiveName, args)

  activeRunId = runId
  target.tool = hook

  let restored = false
  const restore = (): void => {
    if (restored) return
    restored = true
    if (hadPrevious) {
      target.tool = previous
    } else {
      delete target.tool
    }
    if (restoreInstalled === restore) restoreInstalled = undefined
    if (activeRunId === runId) activeRunId = undefined
  }
  restoreInstalled = restore
  return restore
}

/** 恢复 `installToolGlobal` 之前的全局值；未安装时为 no-op。 */
export function uninstallToolGlobal(): void {
  restoreInstalled?.()
}
