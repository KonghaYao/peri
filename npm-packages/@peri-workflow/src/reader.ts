/**
 * 运行结果读取 — 读取宿主落盘的 `.claude/workflow-runs/<runId>/` 并生成报告。
 *
 * 目录产物：
 * - `state.json` — 运行状态与 return_value（超长字符串已被宿主
 *   extract_long_texts 提取为 `outputs/<label>.txt`，此处原位替换回内容）
 * - `outputs/*.txt` — 提取出的长文本
 * - `journal.jsonl` — 每个 agent() 调用的结果（status/tokens/耗时/输出）
 *
 * 纯函数（可单测）与 IO/渲染（reportRun/listRuns）分层：纯函数不依赖
 * process.cwd() 以外的全局状态，测试通过显式传参驱动。
 */
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import type { AgentResult, RunState } from './types'

// ═══════════════════════════════════════════════════════════
// 目录定位与读取（纯函数）
// ═══════════════════════════════════════════════════════════

/** 从 startDir 向上定位 .claude/workflow-runs 根目录；找不到返回 null */
export function findRunsRoot(startDir: string = process.cwd()): string | null {
  let dir = startDir
  for (;;) {
    const candidate = join(dir, '.claude', 'workflow-runs')
    if (existsSync(candidate)) return candidate
    const parent = dirname(dir)
    if (parent === dir) return null
    dir = parent
  }
}

export function loadState(runDir: string): RunState {
  const raw = readFileSync(join(runDir, 'state.json'), 'utf8')
  try {
    return JSON.parse(raw) as RunState
  } catch (e) {
    throw new Error(`state.json 解析失败: ${(e as Error).message}`)
  }
}

/** 读取 outputs/ 全部提取文件 → Map<label, content> */
export function loadOutputs(runDir: string): Map<string, string> {
  const out = new Map<string, string>()
  const dir = join(runDir, 'outputs')
  if (!existsSync(dir)) return out
  for (const f of readdirSync(dir)) {
    if (!f.endsWith('.txt')) continue
    const label = f.slice(0, -'.txt'.length)
    out.set(label, readFileSync(join(dir, f), 'utf8'))
  }
  return out
}

/**
 * 递归遍历 value，把 `${label}` 占位符（宿主 extract_long_texts 产物）
 * 原位替换为 outputs 中对应内容；无对应文件时保留原样。
 */
export function replacePlaceholders(value: unknown, outputs: Map<string, string>): unknown {
  if (typeof value === 'string') {
    const m = /^\$\{([^}]+)\}$/.exec(value)
    if (m && outputs.has(m[1])) return outputs.get(m[1]) as string
    return value
  }
  if (Array.isArray(value)) return value.map((v) => replacePlaceholders(v, outputs))
  if (value && typeof value === 'object') {
    const obj: Record<string, unknown> = {}
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      obj[k] = replacePlaceholders(v, outputs)
    }
    return obj
  }
  return value
}

/** 读取 journal.jsonl（宽容模式：跳过空行/坏行），按 seq 升序 */
export function loadJournal(runDir: string): AgentResult[] {
  const path = join(runDir, 'journal.jsonl')
  if (!existsSync(path)) return []
  const results: AgentResult[] = []
  for (const line of readFileSync(path, 'utf8').split('\n')) {
    const t = line.trim()
    if (!t) continue
    try {
      const entry = JSON.parse(t) as {
        seq?: number
        result?: {
          kind?: string
          output?: unknown
          usage?: { outputTokens?: number }
          toolCount?: number
          tokenCount?: number
          durationMs?: number
          phase?: string
          reason?: string
          detail?: string
        }
      }
      const r = entry.result
      if (!r) continue
      const kind = (r.kind ?? 'ok') as AgentResult['kind']
      results.push({
        seq: entry.seq ?? results.length + 1,
        kind,
        output: r.output,
        tokens: r.tokenCount ?? r.usage?.outputTokens,
        tools: r.toolCount,
        durationMs: r.durationMs,
        phase: r.phase,
        reason: r.reason,
        detail: r.detail,
      })
    } catch {
      // 跳过坏行
    }
  }
  results.sort((a, b) => a.seq - b.seq)
  return results
}

// ═══════════════════════════════════════════════════════════
// 格式化（纯函数）
// ═══════════════════════════════════════════════════════════

export function fmtDuration(start?: string, end?: string): string {
  if (!start || !end) return '-'
  const ms = Date.parse(end) - Date.parse(start)
  if (Number.isNaN(ms)) return '-'
  if (ms < 1000) return `${ms}ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`
  const m = Math.floor(ms / 60_000)
  const s = Math.round((ms % 60_000) / 1000)
  return `${m}m${String(s).padStart(2, '0')}s`
}

export function fmtNum(n?: number): string {
  return n === undefined ? '-' : n.toLocaleString()
}

export function fmtVal(v: unknown): string {
  if (v === undefined || v === null) return '-'
  if (typeof v === 'string') return v
  return JSON.stringify(v)
}

function agentSummary(a: AgentResult): string {
  const out = fmtVal(a.output)
  const first = out.split('\n').find((l) => l.trim().length > 0) ?? ''
  return first.slice(0, 80) || out.slice(0, 80)
}

/** 打印对象 return_value（含占位符替换），返回替换后的对象 */
function renderReturnValue(rv: unknown, outputs: Map<string, string>): unknown {
  const replaced = replacePlaceholders(rv, outputs)
  if (replaced === undefined || replaced === null) {
    console.log('  (无 return value)')
  } else if (typeof replaced === 'string') {
    console.log(replaced)
  } else {
    for (const [k, v] of Object.entries(replaced as Record<string, unknown>)) {
      console.log(`### ${k}\n`)
      if (typeof v === 'string') {
        console.log(v.length > 0 ? v : '  (空)')
      } else {
        console.log(JSON.stringify(v, null, 2))
      }
      console.log()
    }
  }
  return replaced
}

// ═══════════════════════════════════════════════════════════
// 报告生成（IO + 渲染）
// ═══════════════════════════════════════════════════════════

function resolveRunDir(runId: string): string {
  const root = findRunsRoot()
  if (!root) {
    throw new Error('未找到 .claude/workflow-runs 目录（当前目录及其父目录均无）。请在仓库内运行。')
  }
  // 防御性检查：runId 不应包含路径遍历字符（与宿主 journal.rs 一致）
  if (runId.includes('..') || runId.includes('/') || runId.includes('\\')) {
    throw new Error(`非法 runId（含路径字符）: ${runId}`)
  }
  const runDir = join(root, runId)
  if (!existsSync(join(runDir, 'state.json'))) {
    throw new Error(`未找到运行 ${runId}：${runDir}（可用 peri-workflow list 查看已有 run）`)
  }
  return runDir
}

export function reportRun(runId: string, short: boolean, json: boolean): void {
  let runDir: string
  try {
    runDir = resolveRunDir(runId)
  } catch (e) {
    console.error((e as Error).message)
    process.exit(1)
  }

  let state: RunState
  try {
    state = loadState(runDir)
  } catch (e) {
    console.error(`读取运行 ${runId} 失败: ${(e as Error).message}`)
    process.exit(1)
  }
  const outputs = loadOutputs(runDir)
  const agents = loadJournal(runDir)

  if (json) {
    const result = {
      run_id: state.run_id,
      workflow_name: state.workflow_name,
      status: state.status,
      error: state.error ?? null,
      started_at: state.started_at ?? null,
      finished_at: state.finished_at ?? null,
      duration: fmtDuration(state.started_at, state.finished_at),
      return_value: replacePlaceholders(state.return_value, outputs),
      outputs: Object.fromEntries(outputs),
      agents,
      run_dir: runDir,
    }
    console.log(JSON.stringify(result, null, 2))
    return
  }

  console.log(`# Workflow Run ${state.run_id} — ${state.workflow_name}`)
  console.log(`status: ${state.status}${state.error ? ` | error: ${state.error}` : ''}`)
  console.log(`duration: ${fmtDuration(state.started_at, state.finished_at)}`)
  console.log(`run 目录: .claude/workflow-runs/${state.run_id}/\n`)

  if (state.error) {
    console.log(`## Error\n\n${state.error}\n`)
  }

  console.log('## Return value\n')
  if (state.return_value !== undefined && state.return_value !== null) {
    renderReturnValue(state.return_value, outputs)
  } else {
    console.log('  (无 return value)')
  }

  if (agents.length > 0) {
    console.log(`## Agents (${agents.length})\n`)
    console.log('| # | phase | status | tokens | tools | 耗时 | 摘要 |')
    console.log('|---|-------|--------|-------:|------:|-----:|-------|')
    for (const a of agents) {
      const phase = a.phase ?? '-'
      const status = a.kind === 'ok' ? 'ok' : a.kind === 'dead' ? `dead${a.reason ? ` (${a.reason})` : ''}` : 'skipped'
      const dur = a.durationMs === undefined ? '-' : `${(a.durationMs / 1000).toFixed(1)}s`
      console.log(
        `| ${a.seq} | ${phase} | ${status} | ${fmtNum(a.tokens)} | ${fmtNum(a.tools)} | ${dur} | ${agentSummary(a).replace(/\|/g, '\\|')} |`
      )
    }
    if (!short) {
      console.log()
      for (const a of agents) {
        console.log(`--- Agent ${a.seq}${a.phase ? ` (${a.phase})` : ''} [${a.kind}] ---`)
        if (a.kind === 'dead') {
          console.log(`reason: ${a.reason ?? '-'}${a.detail ? `\ndetail: ${a.detail}` : ''}`)
        } else if (a.kind === 'skipped') {
          console.log('(skipped)')
        } else {
          console.log(fmtVal(a.output))
        }
        console.log()
      }
    }
  } else {
    console.log('(journal 为空——无 agent 调用或运行过早失败)')
  }
}

export function listRuns(json: boolean): void {
  const root = findRunsRoot()
  if (!root) {
    console.error('未找到 .claude/workflow-runs 目录')
    process.exit(1)
  }
  const runs: (RunState & { duration: string; dir: string })[] = []
  for (const d of readdirSync(root)) {
    const statePath = join(root, d, 'state.json')
    if (!existsSync(statePath)) continue
    try {
      const st = loadState(join(root, d))
      runs.push({
        ...st,
        duration: fmtDuration(st.started_at, st.finished_at),
        dir: d,
      })
    } catch {
      // 跳过损坏的 state.json
    }
  }
  runs.sort((a, b) => (a.finished_at ?? '').localeCompare(b.finished_at ?? ''))
  if (json) {
    console.log(JSON.stringify(runs, null, 2))
    return
  }
  console.log(`# Workflow runs (${runs.length})\n`)
  console.log('| run_id | workflow | status | 时长 | finished_at |')
  console.log('|--------|----------|--------|------|-------------|')
  for (const r of runs) {
    console.log(`| ${r.run_id} | ${r.workflow_name} | ${r.status} | ${r.duration} | ${r.finished_at ?? '-'} |`)
  }
  console.log('\n读取单个 run：peri-workflow read <run_id>')
}
