# Workflow 内联 script 参数模式导致 "0 agents, 0 tool calls"

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-21

## 问题描述

使用 Workflow 工具的 `script` 参数（内联 JavaScript）派发 workflow 时，返回提示 "0 agents, 0 tool calls"，实际上 workflow 进程已正常启动并执行（耗时与预期一致），但 agent 均未正确注册到追踪器。同样脚本内容改用 `scriptPath` 指向文件后一切正常。`npx`（10.9.3）和 `bun`（1.3.14）均已安装可用。

## 症状详情

| 维度 | inline `script` | `scriptPath` |
|------|:-----:|:------:|
| agent 计数 | 0 | 3 |
| tool call 计数 | 0 | 3 |
| 总耗时 | 15009ms（≈3×5s） | 28582ms |
| journal.jsonl | ❌ 未生成 | ✅ 正常生成 |
| state.json | ❌ 未生成 | ✅ 正常生成 |
| script.js（run 目录） | ✅ 已保存 | ✅ 已保存 |

- 耗时 15009ms 近似等于脚本中 3 个 agent 各执行 5s 的预期时间，说明 Node.js runner 进程本身正常启动并执行
- 但 journal 和 state 文件均未输出，追踪器完全没有记录

## 复现条件

- **复现频率**：必现（单次测试）
- **触发步骤**：
  1. 通过 Workflow 工具派发一个内联 `script` 参数的简单 workflow（如 3 阶段串行 sleep 5s）
  2. 观察返回值：`completed. (NNms, 0 agents, 0 tool calls)`
  3. 检查 run 目录：仅有 `script.js`，无 `journal.jsonl` 和 `state.json`
  4. 将相同脚本保存为 `.mjs` 文件，改用 `scriptPath` 参数重试——正常
- **环境**：macOS 26.5.1，`npx --version`=10.9.3，`bun --version`=1.3.14

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-workflow/src/tool.rs` | WorkflowTool::invoke，script vs scriptPath 统一处理后传入 runner |
| `peri-workflow/src/runner.rs` | WorkflowRunner::run，spawn Node 子进程 |
| `npm-packages/@peri-workflow/runner.ts` | Node.js workflow runner 接收脚本并执行 agent |
| `npm-packages/@peri-workflow/` | RPC 通信协议实现（journal/progress 上报） |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-21 | — | Open | agent | 创建 |
| 2026-07-21 | Open | Fixed | agent | 修复：runner.ts send() 增加 backpressure/drain 处理 + msg_loop 诊断日志 |

## 修复记录

### 修复 #1（2026-07-21）

- **操作人**：agent
- **用户原意**：修复 workflow 内联 script 模式返回 "0 agents, 0 tool calls" 的 bug
- **修复内容**：
  - `npm-packages/@peri-workflow/runner.ts`：将 `send()` 从简单 `process.stdout.write()` 改为带 backpressure 感知的 `waitDrain()`（Node.js `writableNeedDrain` + `once('drain')`），在 `process.exit(0/1)` 前调用排空 stdout 管道。消除中间通知消息（progress/event、journal/append）因管道满被静默丢弃的竞态
  - `peri-workflow/src/runner.rs`：msg_loop 增加消息类型统计计数器 + 退出汇总 tracing 日志，write_state 前后添加诊断日志
  - `peri-workflow/src/rpc.rs`：stdout reader 增加行计数和退出原因日志
- **验证状态**：已验证（inline script 内联测试：2 agents, 2 tool calls，journal.jsonl + state.json 正常生成）
