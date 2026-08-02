/**
 * CLI 子命令分发 — read / list / validate / help 的 argv 解析与调用。
 *
 * 与 index.ts 分离以便单测：本模块无进程级副作用
 * （read/list 的成功路径不触发 process.exit）。
 */
import { readFileSync } from 'node:fs'
import { listRuns, reportRun } from './reader'
import { validateScript } from './validate'

export function cliUsage(): void {
  console.log(`用法（CLI 子命令）:
  peri-workflow read <runId> [--short] [--json]   # 完整报告（state + return_value + agents 全量输出）
  peri-workflow list [--json]                     # 列出所有 run（按结束时间倒序）
  peri-workflow validate <script.mjs> [--json]    # 校验 workflow 脚本语法（引擎检查 + 静态补充）
  peri-workflow --help                            # 本帮助

无参数时以 JSON-RPC 模式运行（宿主集成，见 DESIGN.md）。
read/list 从当前目录向上自动定位 .claude/workflow-runs/。`)
}

/** 首参是否命中 CLI 子命令（read/list/validate/help） */
export function isCliCommand(cmd: string | undefined): boolean {
  return (
    cmd === 'read' ||
    cmd === 'list' ||
    cmd === 'validate' ||
    cmd === '--help' ||
    cmd === '-h' ||
    cmd === 'help'
  )
}

export function cliMain(args: string[]): void {
  const cmd = args[0]
  if (cmd === 'read') {
    const runId = args.slice(1).find((a) => !a.startsWith('--'))
    if (!runId) {
      console.error('用法：peri-workflow read <runId> [--short] [--json]（--help 查看更多）')
      process.exit(1)
    }
    reportRun(runId, args.includes('--short'), args.includes('--json'))
  } else if (cmd === 'list') {
    listRuns(args.includes('--json'))
  } else if (cmd === 'validate') {
    validateFile(args.slice(1).find((a) => !a.startsWith('--')), args.includes('--json'))
  } else {
    cliUsage()
    process.exit(0)
  }
}

/** validate 子命令：读取脚本文件 → 校验 → 文本或 JSON 输出，失败 exit 1 */
export function validateFile(file: string | undefined, json: boolean): void {
  if (!file) {
    console.error('用法：peri-workflow validate <script.mjs> [--json]（--help 查看更多）')
    process.exit(1)
  }
  let source: string
  try {
    source = readFileSync(file, 'utf8')
  } catch {
    console.error(`无法读取文件: ${file}`)
    process.exit(1)
  }

  const r = validateScript(source)

  if (json) {
    console.log(
      JSON.stringify(
        {
          file,
          ok: r.ok,
          meta: r.meta,
          errors: r.errors.map((e) => e.message),
          warnings: r.warnings.map((e) => e.message),
        },
        null,
        2,
      ),
    )
    if (!r.ok) process.exit(1)
    return
  }

  if (r.ok && r.warnings.length === 0) {
    const name = r.meta?.name ? ` (${r.meta.name})` : ''
    console.log(`✓ ${file} 校验通过${name}`)
    return
  }
  if (r.ok) {
    console.log(`✓ ${file} 校验通过（${r.warnings.length} 个警告）：`)
    for (const w of r.warnings) console.log(`  ⚠ ${w.message}`)
    return
  }
  console.log(`✗ ${file} 校验失败（${r.errors.length} 个错误）：`)
  for (const e of r.errors) console.log(`  ✗ ${e.message}`)
  for (const w of r.warnings) console.log(`  ⚠ ${w.message}`)
  process.exit(1)
}
