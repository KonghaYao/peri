# P1：Workflow 重复结果回填重复计入 maxToolCalls 导致误报超限

**状态**：已修复，待验证。
**优先级**：P1。
**类型**：错误（限额计数）。
**创建日期**：2026-11-06。
**来源**：GitHub issue `KonghaYao/peri#201`。
**确认方式**：主 agent 复核静态调用链 + 运行时复现（基线 `fbb1d8ac0`）。

## 确认证据

- 触发：`WorkflowTaskTable::settle` 对已终态任务再次回填时走幂等分支并再次
  `notify_terminal`（`peri-workflow/src/runtime/tasks.rs:416-435`），这是投递重试入口。
- 机制：终态 sink 每次通知都执行 `note_task_tool_calls`
  （`peri-workflow/src/runtime.rs:238-252`）。
- 放大：`RunLimitState::observe_tool_calls` 只做无身份 `fetch_add`
  （`peri-workflow/src/runner/limits.rs:39-51`），没有按 task 去重。
- 运行时复现（修复前）：同一 limit 状态（`maxToolCalls=5`）对同一任务连续观察
  两次 3 次工具调用，第二次返回
  `Some("workflow exceeded maxToolCalls (5)")`；同时任务表 sink 在两次 `settle`
  下被调用两次（临时用例输出已验证，修复后固化为回归用例）。

## 根因链

1. Node 侧结果投递重试（或引擎重发 `agent/result`）再次回填同一 task 结果。
2. 任务表按 design §5.1 保留首次结果并重新通知 sink，用于重试投递。
3. sink 把每次通知都当作一次新的执行结果累计 tool 调用数。
4. 用户可见后果：实际未超限的 run 在 `workflow/done` 投影为
   `status = failed` + `workflow exceeded maxToolCalls (N)`，交付被误判失败。

## 影响面

- 仅影响设置了 `maxToolCalls` 的 workflow run；重投发生在投递失败或引擎重试时。
- 结果投递本身不受影响，Node 仍按 taskId 去重；错误只在 run 终态投影与限额判定。
- 无数据库 schema、协议或持久格式变化。

## 修复方案

- `RunLimitState` 增加按 `taskId` 的去重集合：同一任务只计首次终态结果；
  无 `maxToolCalls` 时不保留去重身份（不产生 breach，无需计数）。
- `WorkflowRunner::note_task_tool_calls` / 终态 sink 透传 `task_id`；投递重试行为不变。
- 回归：
  - `runner::tests::run_limit_state_counts_each_task_once_on_redelivery`（重投只计一次）
  - `runner::tests::concurrent_redelivery_of_one_task_counts_once`（并发重投只计一次）
  - `runner::tests::duplicate_task_backfill_counts_tool_calls_once`（首次正常计数）
  - `runtime::tasks::tests::duplicate_settle_redelivers_the_same_terminal_result`
    （重投仍会再次投递）

## 验证记录

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-workflow --lib -- run_limit_state duplicate_task_backfill concurrent_redelivery duplicate_settle_redelivers`
  → 5 passed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-workflow --lib`
  → 191 passed；3 个 artifact fail-closed 用例在本机失败，基线 `fbb1d8ac0`（未改动的
  主 checkout）同样失败，属本机已缓存 workflow artifact 的环境差异。
- `./scripts/check-changed.sh` → 通过。
- `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-workflow --all-targets -- -W clippy::all`
  → 通过（仅上游 patched `hyper-util` 既有 warning）。
