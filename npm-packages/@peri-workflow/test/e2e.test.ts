/**
 * e2e 模拟测试 — spawn 构建产物 dist/peri-workflow.js，模拟宿主完整交互。
 *
 * 1. **JSON-RPC 模式**：模拟宿主进程 — 发 workflow/start、响应 agent/run、
 *    断言 progress/journal/workflow/done 全事件链（真实 engine 执行）。
 * 2. **CLI 子命令模式**：构造临时 workflow-runs 目录，验证 read/list 输出与错误路径。
 *
 * 前置：测试会自动执行 `bun run build`（保证 dist 为最新源码产物）。
 */
import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import { mkdtempSync, mkdirSync, writeFileSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), '..')
const distPath = join(pkgRoot, 'dist', 'peri-workflow.js')

// ─── 工具 ──────────────────────────────────────────────────

function buildDist(): void {
  const r = Bun.spawnSync({
    cmd: ['bun', 'run', 'build'],
    cwd: pkgRoot,
    stdout: 'pipe',
    stderr: 'pipe',
  })
  if (!r.success) {
    throw new Error(`build failed: ${r.stderr.toString()}`)
  }
}

/** 一次性 CLI 调用：返回 exit code + stdout/stderr */
async function runCli(
  args: string[],
  opts?: { cwd?: string }
): Promise<{ code: number; stdout: string; stderr: string }> {
  const proc = Bun.spawn({
    cmd: [process.execPath, distPath, ...args],
    cwd: opts?.cwd ?? pkgRoot,
    stdin: 'ignore',
    stdout: 'pipe',
    stderr: 'pipe',
  })
  const [stdout, stderr] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
  ])
  return { code: await proc.exited, stdout, stderr }
}

interface RpcSession {
  send: (msg: unknown) => void
  waitFor: (pred: (m: Record<string, unknown>) => boolean, timeoutMs?: number) => Promise<Record<string, unknown>>
  all: () => Record<string, unknown>[]
  kill: () => void
}

/** JSON-RPC 交互会话：模拟宿主 spawn 二进制并通过 stdin/stdout 对话 */
function startRpc(): RpcSession {
  const proc = Bun.spawn({
    cmd: [process.execPath, distPath],
    cwd: pkgRoot,
    stdin: 'pipe',
    stdout: 'pipe',
    stderr: 'pipe',
  })
  const lines: Record<string, unknown>[] = []

  void (async () => {
    const decoder = new TextDecoder()
    let buf = ''
    const reader = proc.stdout.getReader()
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      buf += decoder.decode(value, { stream: true })
      let idx: number
      while ((idx = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, idx).trim()
        buf = buf.slice(idx + 1)
        if (!line) continue
        try {
          lines.push(JSON.parse(line))
        } catch {
          // 忽略非 JSON 行
        }
      }
    }
  })()

  return {
    send(msg: unknown) {
      // bun 的 proc.stdin 是 FileSink（write + flush，无 getWriter）
      proc.stdin.write(JSON.stringify(msg) + '\n')
      proc.stdin.flush()
    },
    async waitFor(pred, timeoutMs = 8000) {
      const deadline = Date.now() + timeoutMs
      for (;;) {
        const found = lines.find(pred)
        if (found) return found
        if (Date.now() > deadline) {
          throw new Error(`waitFor timeout; seen=${JSON.stringify(lines)}`)
        }
        await new Promise((r) => setTimeout(r, 25))
      }
    },
    all: () => [...lines],
    kill: () => proc.kill(),
  }
}

const sessions: RpcSession[] = []

afterAll(() => {
  for (const s of sessions) s.kill()
})

beforeAll(() => {
  buildDist()
})

// ═══════════════════════════════════════════════════════════
// JSON-RPC 模式 — 模拟宿主完整交互
// ═══════════════════════════════════════════════════════════

describe('JSON-RPC 模式（宿主模拟）', () => {
  test('workflow/start → agent/run → 响应 → workflow/done 全链路', async () => {
    const s = startRpc()
    sessions.push(s)

    const script = `export const meta = { name: 'e2e-demo', description: 'e2e test' }
phase('run')
const r = await agent('hello agent', { agentType: 'web-researcher' })
return { answer: r }`

    s.send({
      jsonrpc: '2.0',
      id: 1,
      method: 'workflow/start',
      params: { runId: 'e2e-run-1', cwd: '/tmp', script, budgetTotal: null },
    })

    // start 同步响应
    const startResp = await s.waitFor((m) => m.id === 1 && 'result' in m)
    expect(startResp.result).toEqual({
      ok: true,
      protocolVersion: 1,
      buildId: '@peri-code/workflow@0.2.0',
    })

    // 事件链前置
    await s.waitFor((m) => (m.method as string) === 'progress/event' && (m.params as { type: string }).type === 'run_started')

    // 收到 agent/run 请求 → 模拟宿主响应 ok
    const agentReq = await s.waitFor((m) => m.method === 'agent/run')
    expect((agentReq.params as { runId: string }).runId).toBe('e2e-run-1')
    s.send({
      jsonrpc: '2.0',
      id: agentReq.id,
      result: { kind: 'ok', output: '宿主模拟结果', usage: { outputTokens: 42 } },
    })

    // 终态
    const done = await s.waitFor(
      (m) => (m.method as string) === 'workflow/done',
      15000
    )
    const params = done.params as { status: string; returnValue: { answer: string } }
    expect(params.status).toBe('completed')
    expect(params.returnValue).toEqual({ answer: '宿主模拟结果' })

    // 事件序列包含关键节点
    const methods = s.all().map((m) => (m.method as string) ?? `resp:${m.id}`)
    expect(methods).toContain('progress/event')
    expect(methods).toContain('journal/append')
    const progressTypes = s
      .all()
      .filter((m) => m.method === 'progress/event')
      .map((m) => (m.params as { type: string }).type)
    expect(progressTypes).toEqual(
      expect.arrayContaining(['run_started', 'phase_started', 'agent_started', 'agent_done', 'phase_done', 'run_done'])
    )
  })

  test('budgetTotal 在首个 agent 消耗额度后阻止后续 agent', async () => {
    const s = startRpc()
    sessions.push(s)

    const script = `export const meta = { name: 'e2e-budget', description: 'budget e2e test' }
const first = await agent('first')
const second = await agent('second')
return { first, second }`

    s.send({
      jsonrpc: '2.0',
      id: 1,
      method: 'workflow/start',
      params: { runId: 'e2e-budget-1', cwd: '/tmp', script, budgetTotal: 1 },
    })
    await s.waitFor((m) => m.id === 1 && 'result' in m)

    const first = await s.waitFor((m) => m.method === 'agent/run')
    s.send({
      jsonrpc: '2.0',
      id: first.id,
      result: { kind: 'ok', output: 'first-result', usage: { outputTokens: 1 } },
    })

    const done = await s.waitFor((m) => m.method === 'workflow/done', 10000)
    const params = done.params as { status: string; error?: string }
    expect(params.status).toBe('failed')
    expect(
      s.all().filter((m) => m.method === 'agent/run')
    ).toHaveLength(1)

    const runDone = s
      .all()
      .filter((m) => m.method === 'progress/event')
      .map((m) => m.params as { type: string; status?: string })
      .find((event) => event.type === 'run_done')
    expect(runDone?.status).toBe('failed')
  })

  test('budgeted resume 命中 journal 时不重复 agent 调用', async () => {
    const s = startRpc()
    sessions.push(s)

    const script = `export const meta = { name: 'e2e-budget-resume', description: 'budget resume e2e test' }
const result = await agent('cached')
return { result }`
    const key = createHash('sha256')
      .update('cached\n' + JSON.stringify({ prompt: 'cached' }))
      .digest('hex')

    s.send({
      jsonrpc: '2.0',
      id: 1,
      method: 'workflow/start',
      params: {
        runId: 'e2e-budget-resume-1',
        cwd: '/tmp',
        script,
        budgetTotal: 1,
        resume: [{
          key,
          seq: 0,
          result: { kind: 'ok', output: 'cached-result', usage: { outputTokens: 99 } },
        }],
      },
    })
    await s.waitFor((m) => m.id === 1 && 'result' in m)

    const done = await s.waitFor((m) => m.method === 'workflow/done', 10000)
    const params = done.params as { status: string; returnValue: { result: string } }
    expect(params.status).toBe('completed')
    expect(params.returnValue).toEqual({ result: 'cached-result' })
    expect(s.all().filter((m) => m.method === 'agent/run')).toHaveLength(0)
    expect(s.all().filter((m) => m.method === 'journal/append')).toHaveLength(0)
  })

  test('未知方法返回 -32601', async () => {
    const s = startRpc()
    sessions.push(s)
    s.send({ jsonrpc: '2.0', id: 9, method: 'no/such/method' })
    const resp = await s.waitFor((m) => m.id === 9)
    expect((resp.error as { code: number }).code).toBe(-32601)
  })

  test(
    'workflow/kill 中止运行中的 workflow',
    async () => {
      const s = startRpc()
      sessions.push(s)

      // engine 在 agent 调用间隙检查 abort signal：首个 agent 完成后 kill
      const script = `export const meta = { name: 'e2e-kill', description: 'e2e kill test' }
const r1 = await agent('first')
const r2 = await agent('second')
return { r1, r2 }`

      s.send({
        jsonrpc: '2.0',
        id: 1,
        method: 'workflow/start',
        params: { runId: 'e2e-kill-1', cwd: '/tmp', script, budgetTotal: null },
      })
      await s.waitFor((m) => m.id === 1 && 'result' in m)

      // 响应首个 agent 后立即 kill
      const first = await s.waitFor((m) => m.method === 'agent/run')
      s.send({
        jsonrpc: '2.0',
        id: first.id,
        result: { kind: 'ok', output: 'r1', usage: { outputTokens: 1 } },
      })
      s.send({ jsonrpc: '2.0', id: 2, method: 'workflow/kill' })
      const killResp = await s.waitFor((m) => m.id === 2)
      expect(killResp.result).toEqual({ ok: true })

      const done = await s.waitFor((m) => m.method === 'workflow/done', 10000)
      const status = (done.params as { status: string }).status
      expect(status).toBe('killed')
    },
    { timeout: 25000 }
  )
})

// ═══════════════════════════════════════════════════════════
// CLI 子命令模式 — 真实文件系统 + 进程调用
// ═══════════════════════════════════════════════════════════

/**
 * CLI 子命令模式的测试 fixture — 构造真实落盘目录结构。
 *
 * ⚠ 跨侧契约：以下字段与 Rust 侧 `peri-workflow/src/journal.rs` 输出逐字段对齐
 * （依据：DESIGN.md「运行结果落盘格式」节，单一事实源）：
 * - state.json：RunState 直出（snake_case）— run_id/workflow_name/status/
 *   return_value/script/started_at/finished_at（error 可选省略）
 * - journal.jsonl：每行 JournalEntry{ key, seq, result }；result 为 camelCase wire
 *   的 AgentRunResult（ok 变体：kind/output/usage.outputTokens，toolCount 等可选省略）
 * - outputs/<label>.txt + `${label}` 占位符：extract_long_texts 的提取形态
 *   （顶层字段路径 label，原位替换为 `${label}`）
 * Rust 侧改动落盘格式时，须同步本 fixture 与 reader.ts。
 */
function makeRunsRoot(): string {
  const base = mkdtempSync(join(tmpdir(), 'workflow-e2e-'))
  const root = join(base, '.claude', 'workflow-runs')
  const runDir = join(root, 'run-e2e')
  mkdirSync(join(runDir, 'outputs'), { recursive: true })
  writeFileSync(
    join(runDir, 'state.json'),
    JSON.stringify({
      run_id: 'run-e2e',
      workflow_name: 'e2e-cli',
      status: 'completed',
      return_value: { summary: '${fix}' },
      script: '',
      started_at: '2026-08-02T00:00:00Z',
      finished_at: '2026-08-02T00:00:59Z',
    })
  )
  writeFileSync(join(runDir, 'outputs', 'fix.txt'), 'CLI 长文本')
  writeFileSync(
    join(runDir, 'journal.jsonl'),
    JSON.stringify({
      key: 'k',
      seq: 0,
      result: { kind: 'ok', output: 'agent-out', usage: { outputTokens: 10 } },
    }) + '\n'
  )
  return base
}

describe('CLI 子命令模式', () => {
  test('read --json：占位符替换 + 结构化输出', async () => {
    const cwd = makeRunsRoot()
    const r = await runCli(['read', 'run-e2e', '--json'], { cwd })
    expect(r.code).toBe(0)
    const parsed = JSON.parse(r.stdout)
    expect(parsed.run_id).toBe('run-e2e')
    expect(parsed.status).toBe('completed')
    expect(parsed.duration).toBe('59.0s')
    expect(parsed.return_value).toEqual({ summary: 'CLI 长文本' })
    expect(parsed.agents[0].tokens).toBe(10)
  })

  test('read --short：表格报告', async () => {
    const cwd = makeRunsRoot()
    const r = await runCli(['read', 'run-e2e', '--short'], { cwd })
    expect(r.code).toBe(0)
    expect(r.stdout).toContain('# Workflow Run run-e2e — e2e-cli')
    expect(r.stdout).toContain('| # | phase | status | tokens | tools | 耗时 | 摘要 |')
  })

  test('list：列出所有 run', async () => {
    const cwd = makeRunsRoot()
    const r = await runCli(['list'], { cwd })
    expect(r.code).toBe(0)
    expect(r.stdout).toContain('# Workflow runs (1)')
    expect(r.stdout).toContain('| run-e2e | e2e-cli | completed |')
  })

  test('read 非法 runId：exit 1 + 拒绝信息', async () => {
    const r = await runCli(['read', '../evil'])
    expect(r.code).toBe(1)
    expect(r.stderr).toContain('非法 runId')
  })

  test('read 不存在的 run：exit 1 + 提示', async () => {
    const cwd = makeRunsRoot()
    const r = await runCli(['read', 'ghost-run'], { cwd })
    expect(r.code).toBe(1)
    expect(r.stderr).toContain('未找到运行 ghost-run')
  })

  test('workflow-runs 目录不存在：exit 1', async () => {
    const empty = mkdtempSync(join(tmpdir(), 'workflow-no-runs-'))
    const r = await runCli(['list'], { cwd: empty })
    expect(r.code).toBe(1)
    expect(r.stderr).toContain('未找到 .claude/workflow-runs')
  })

  test('--help：exit 0 + 用法', async () => {
    const r = await runCli(['--help'])
    expect(r.code).toBe(0)
    expect(r.stdout).toContain('用法（CLI 子命令）')
  })

  test('validate 合法脚本：exit 0 + ✓ 输出', async () => {
    const base = mkdtempSync(join(tmpdir(), 'workflow-validate-'))
    const good = join(base, 'good.mjs')
    writeFileSync(
      good,
      `export const meta = { name: 'e2e-ok', description: 'ok' }\nconst r = await agent('hi')\nreturn r`
    )
    const r = await runCli(['validate', good])
    expect(r.code).toBe(0)
    expect(r.stdout).toContain('✓')
    expect(r.stdout).toContain('e2e-ok')
  })

  test('validate 坏脚本（workflow.agent 旧式调用）：exit 1 + 修复指引', async () => {
    const base = mkdtempSync(join(tmpdir(), 'workflow-validate-'))
    const bad = join(base, 'bad.mjs')
    writeFileSync(
      bad,
      `export const meta = { name: 'e2e-bad', description: 'ok' }\nconst r = await workflow.agent('hi')\nreturn r`
    )
    const r = await runCli(['validate', bad])
    expect(r.code).toBe(1)
    expect(r.stdout).toContain('✗')
    expect(r.stdout).toContain('workflow.agent(')
  })

  test('validate 文件不存在：exit 1 + 提示', async () => {
    const r = await runCli(['validate', '/no/such/file.mjs'])
    expect(r.code).toBe(1)
    expect(r.stderr).toContain('无法读取文件')
  })
})
