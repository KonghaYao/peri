#!/usr/bin/env node
/**
 * @peri-code/workflow — 入口：argv 分发（CLI 子命令 or JSON-RPC 模式）。
 *
 * ## 双模式
 *
 * 1. **JSON-RPC 模式（默认，无 argv 参数）**：宿主 spawn 本进程后通过 stdin/stdout
 *    对话，执行 workflow 编排（协议见 DESIGN.md）。
 *
 * 2. **CLI 子命令模式**：直接以命令参数运行，读取已落盘的运行结果
 *    （`.claude/workflow-runs/<runId>/` 下的 state.json / outputs / journal.jsonl）：
 *    - `peri-workflow read <runId> [--short] [--json]` — 完整报告（含占位符原位替换）
 *    - `peri-workflow list [--json]` — 列出所有运行
 *    - `peri-workflow --help` — 用法
 *    运行目录从当前目录向上自动定位 `.claude/workflow-runs/`。
 */
import { cliMain, isCliCommand } from './cli'
import { startJsonRpc } from './jsonrpc'

const args = process.argv.slice(2)

if (isCliCommand(args[0])) {
  cliMain(args)
} else {
  startJsonRpc()
}
