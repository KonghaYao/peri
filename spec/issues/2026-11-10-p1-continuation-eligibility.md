# P1：查询重试耗尽后一次性续跑资格丢失

**状态**：已修复，待 PR/CI 验收。
**优先级**：P1。
**类型**：错误（回归 / 核心功能）。
**创建日期**：2026-11-10。
**来源**：4.6.5 → 4.7.0 审查 + explorer 只读确认。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `a7442924`）。

## 确认证据

- 取消前台 prompt 时递增代际、抑制自动激活并置位一次性资格：
  `peri-acp/src/host/notify.rs:77`（`continuation_epoch += 1`）、
  `:84`（`activation.suppress()`）、
  `:92-109`（有 `cancel_token` 时 `continuation_armed = true`）。
- 子 Agent 完成时 listener 因 `can_activate=false` 走 armed 分支，发出非
  mq steering 请求：`peri-acp/src/host/activation.rs:99-113`、`:114-121`
  （`ContinuationRequest { mq_steering: false, pending_result: false }`）。
- scheduler 原子 take 并立即清除 armed：
  `peri-acp/src/host/continuation.rs:449-455` →
  `take_continuation_for_request`（`:76-95`）→
  `take_continuation_if_armed`（`:64-73`，`:71` 清除 `continuation_armed`）。
- dispatch 二次资格查询经 `checked_dispatch_query` 重试，预算 1s：
  `peri-acp/src/host/prompt_dispatch.rs:227-289`。
- 预算耗尽走 `:276-283` → `abort_continuation_query_error`（`:725-746`），
  只调用 `queue.request_recheck()`（`:739-740`），返回 `Null`，不恢复 armed。
- listener 重查时 `armed=false` 且 `allowed=false`，结论为 `Idle`
  （`peri-acp/src/host/activation.rs:192-230`）；Defer 留在 MQ，直到用户新输入。
- 结论：P1 成立。#380 改善了查询失败后的唤醒，但漏掉“查询失败不得消耗一次性
  续跑资格”。

## 根因链

1. 用户取消前台 prompt → `continuation_epoch += 1`、`activation.suppress()`、
   `continuation_armed = true`（观察：notify.rs）。
2. bg 子 Agent 完成 → listener 看到 armed 且无法自动激活，发出唯一一次
   `mq_steering=false` 续跑请求（观察：activation.rs）。
3. scheduler `take_continuation_if_armed` 原子清除 armed 并返回 epoch
   （观察：continuation.rs）。
4. dispatch 在准入后二次查询 `has_pending_defer(SubAgentComplete)`；存储瞬时
   不可用，1s 预算内重试耗尽（观察：prompt_dispatch.rs）。
5. 失败分支只 `request_recheck()`，不恢复 armed；listener 重查时 armed 已为
   false，只能返回 `Idle`（观察：activation.rs）。
6. 用户可见后果：已完成的子 Agent 结果滞留在 MQ，父 agent 不会自动续跑，
   直到用户下一次输入；取消后的唯一一次自动续跑资格被查询故障静默吞掉
   （观察 + 推断：Defer 残留由测试复现确认）。

## 影响面

- 用户影响：取消前台后子 Agent 完成的通知不再自动消费，表现为“取消后子任务
  结果不回来”；需要用户再发一条消息才处理。
- 受影响模块/调用点：`peri-acp` host 续跑调度（notify / activation /
  continuation / prompt_dispatch）。
- 是否可规避：用户再次输入任意 prompt 会消费滞留 Defer。

## 修复方案

- 在 `peri-acp/src/host/continuation.rs` 增加纯函数
  `restore_continuation_armed(state, epoch) -> bool`：
  仅当 epoch 未变（复用 `continuation_still_valid`）、`!closing`、
  `close_in_flight == 0`、`!continuation_in_flight` 时置回
  `continuation_armed = true`，返回是否恢复。
- 仅在 `prompt_dispatch.rs` 的 `subagent_defer` 查询 Err 分支（armed 续跑请求，
  即 `PromptOrigin::Continuation { mq_steering: false, pending_result: false }`）
  调用恢复助手，并且**先恢复 armed，再 `request_recheck()`**，避免 listener
  先醒后睡。
- 增加可观察 `warn!`：`session_id`、`epoch`、`query`、`restored`。
- 明确不做：
  - 不修改 `take_continuation_if_armed` 的原子清除 / coalesce 语义；
  - 不改 listener `Retry` 与 `checked_dispatch_query` 的预算；
  - 不改 `mq_continuation` / `mq_steering_allowed` 的 abort 分支；
  - 不把 take 移入 dispatch；
  - 不改变用户新 prompt / 再次 Stop 的作废逻辑（epoch 相等校验天然失效）；
  - 不做 schema 变更，不扩无关文件。
- 回归测试：
  - `peri-acp/src/host/continuation_test.rs`：同 epoch 恢复成功；
    epoch 变化 / closing / close_in_flight / in_flight 不恢复；恢复后新 prompt /
    再次 Stop 使旧代际失效。
  - `peri-acp/src/host/activation_test.rs`：取消 → 子任务完成 → 首个 armed 请求
    → take → 准入后注入 `thread_mq` 缺失 → dispatch 预算耗尽 → 恢复表 →
    listener 自动发出第二个 armed 请求 → take 返回同一 epoch → 再 dispatch
    得到 `end_turn` 且 MQ 清空；新 prompt 后 listener 不复活旧代际请求。

## 验收标准

1. 取消后子 Agent 完成，即使 dispatch 二次查询耗尽预算，无需用户输入也会自动
   消费滞留结果并结束本轮。
2. 恢复只发生在同一代际且会话未关闭、无续跑在飞时；新 prompt / 再次 Stop /
   关闭不会复活旧资格。
3. 目标单测与 `peri-acp --lib` 通过，format / clippy / `git diff --check` 通过。

## 验证记录

修复前复现（仅回退 `prompt_dispatch.rs` 的恢复调用，保留助手与用例）：

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- armed_continuation_query_failure_restores_eligibility_for_listener`
  → 1 failed，5.09s，panic：`查询失败必须恢复 armed 资格: Elapsed(())`。

修复后收尾实跑（分支 `fix/continuation-eligibility`，基线 `a7442924`）：

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::activation`
  → 0 passed, 0 failed（0 matched，1238 filtered），exit 0。
  该过滤路径不存在：`host/activation_test.rs` 实际挂在
  `host::requests::tests::activation_tests`；`session::activation` 是另一测试模块。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::continuation`
  → 19 passed, 0 failed（1219 filtered），0.02s，含新增 3 个恢复/失效单测。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- host::requests::tests::activation_tests`
  → 32 passed, 0 failed（1206 filtered），4.89s，含新增 2 个集成用例：
  `armed_continuation_query_failure_restores_eligibility_for_listener`、
  `restored_armed_is_not_reactivated_after_new_prompt`。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib`
  → 1237 passed, 0 failed, 1 ignored，81.26s，exit 0。
- `./scripts/cargo-rmcp-patched.sh fmt --all -- --check` → exit 0。
- `./scripts/cargo-rmcp-patched.sh clippy --locked --all-targets -- -D warnings`
  → exit 0，无 warning（workspace 全 targets）。
- `git diff --check` → exit 0。

## 风险与明确不做

- 故障注入方式为准入后重命名 SQLite `thread_mq` 表，覆盖查询重试预算耗尽路径；
  未做真实进程崩溃（kill -9 / 磁盘故障）与并发压力测试。
- 只在 `subagent_defer` 查询失败分支恢复资格；`mq_steering`、`pending_result`、
  Scheduled 等其它 abort 分支不消费一次性 armed，按范围保持原样。
- 未验证远端 MQ 后端；恢复依赖后端把查询失败作为 `Err` 上报，本地 SQLite 路径
  已验证。恢复后 listener 仍受既有 50ms–1s 退避约束，故障持续时会重复
  「恢复 → 重查 → 失败」，不新增额外重试预算。
- 不改 `take_continuation_if_armed` 的原子清除/coalesce 语义、listener 重试预算、
  用户新 prompt / 再次 Stop 的作废逻辑，不新增 schema。
- #380 的既有性质不变：它补齐查询失败后的 listener 唤醒，本次只补「失败不得消耗
  一次性续跑资格」，其余唤醒/退避逻辑沿用 #380。

