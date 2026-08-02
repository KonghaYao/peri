/**
 * CLI 子命令分发 — read / list / help 的 argv 解析与调用。
 *
 * 与 index.ts 分离以便单测：本模块无进程级副作用
 * （read/list 的成功路径不触发 process.exit）。
 */
import { listRuns, reportRun } from './reader'

export function cliUsage(): void {
  console.log(`用法（CLI 子命令）:
  peri-workflow read <runId> [--short] [--json]   # 完整报告（state + return_value + agents 全量输出）
  peri-workflow list [--json]                     # 列出所有 run（按结束时间倒序）
  peri-workflow --help                            # 本帮助

无参数时以 JSON-RPC 模式运行（宿主集成，见 DESIGN.md）。
从当前目录向上自动定位 .claude/workflow-runs/。`)
}

/** 首参是否命中 CLI 子命令（read/list/help） */
export function isCliCommand(cmd: string | undefined): boolean {
  return cmd === 'read' || cmd === 'list' || cmd === '--help' || cmd === '-h' || cmd === 'help'
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
  } else {
    cliUsage()
    process.exit(0)
  }
}
