# P1：已终态 child 在父会话 load/resume 时被冷恢复并跑一轮

**状态**：已修复，待 PR/CI 验收。
**优先级**：P1。
**类型**：回归（恢复准入 / 工作意图一致性）。
**创建日期**：2026-11-10。
**来源**：用户报告 + explorer 只读代码审计。
**确认方式**：explorer 只读确认 + 主 agent 复核静态调用链与本地库取证（基线 `a7442924`）。

## 确认证据

- 恢复范围只按 `work_state` 选候选：`load_recovery_scope` → `RECOVERY_SCOPE_FILTER`
  只过滤 `t.work_state = 'working'`
  （`peri-acp/src/host/recovery/coordinator.rs:125-130`、
  `peri-resources/src/sessions/sqlite_store/row_mapping.rs:33`）。
- 逐 thread 复核也只查工作意图：`prepare_root`
  （`peri-acp/src/host/recovery/coordinator.rs:223-238`）与 `prepare_child`
  （同文件 `:275-286`、`:305-316`）在 `agent_status` 为
  `Done`/`Cancelled`/`Error` 时仍准入；终态判定已存在
  （`peri-acp-types/src/thread/types.rs:81-85`）。
- 准入后向 child MQ 注入恢复提醒（`peri-acp/src/host/recovery/coordinator/admission.rs:177-190`），
  prompt 分发再把 `work_state` 写成 `Working`
  （`peri-acp/src/host/prompt_dispatch.rs:467-475`），child 跑完一轮并发出
  `Recovered child thread ... finished a turn`（`recovered: true`，
  `peri-acp/src/host/recovery/parent_notice.rs:262-270`）。
- 终态 + `working` 无法被后续结算修复：`finalize_child_terminal_mq` 在
  `current.is_terminal()` 时跳过终态写入并返回 no-op
  （`peri-resources/src/sessions/sqlite_store/session_data/mq.rs:691-716`）；
  `settle_terminal_without_result_rows` 同样直接返回
  （同文件 `:742-748`）。
- 消息接纳把任意 thread 的 `idle` 无条件提升为 `working`
  （同文件 `:232-249`），不区分 `agent_status`。
- `ClaimDecision::Finish` / `CancelRunning` 用只写 `agent_status` 的
  `update_session_meta`（`peri-agent/src/session/subagent/factory/claim.rs:240-288`；
  SQL 见 `peri-resources/src/sessions/sqlite_store/session_data/lifecycle.rs:153-205`），
  会留下「终态 + `working`」。
- 本地库取证（`~/.peri/threads/threads.db` 经 APFS 克隆到 `/tmp` 后只读查询，
  未改动线上库）：

  | thread id | parent_thread_id | agent_status | work_state | updated_at | archived |
  | --- | --- | --- | --- | --- | --- |
  | `01a1c0f7-7255-7e61-ba33-be4c5f15f92b` | `01a1bea4-3e19-7ae0-9691-e0929616867e` | `error` | `working` | `2026-11-10T06:13:40.878665+00:00` | `0` |
  | `01a1c33c-5dc4-7b10-a2c4-ef72870a32cc` | `01a1bea4-3e19-7ae0-9691-e0929616867e` | `error` | `working` | `2026-11-10T06:14:11.077208+00:00` | `0` |

  两个 child 的 `thread_mq` 均无残留行（提醒已被消费），历史中各有一条
  `source = session_recovery` 的 `system_reminder`（消息数 214 / 311），
  与「冷恢复注入 → 跑一轮 → 终态 + 残留 working」一致。

## 根因链

1. `ClaimDecision::Finish` / `CancelRunning`（以及其它只写 `agent_status` 的收尾）
   提交终态后没有同步 `work_state`，留下 `Done|Cancelled|Error + working`。
2. 恢复范围 SQL 只按 `work_state = 'working'` 选候选，逐 thread 复核同样只看
   `work_state`，终态 child 仍是候选。
3. `prepare_child` 为它冷装配运行实例并注入 recovery reminder。
4. prompt 分发写入 `Working` 后跑一轮；turn 结束再写一次终态，同时父会话收到
   `recovered: true` 的「子任务已完成」通知。
5. 用户可见后果：每次 load/resume 父会话，已结束的 child 被无故唤醒、重复消耗
   模型调用，并向父会话重复投递「后台任务结果」。

观察与推断：1–4 由代码路径与取证结果直接支持；「历史提醒是在冷恢复轮被消费」
来自 MQ 无残留 + 历史存在 `session_recovery` 提醒这一组合事实。

## 同族问题：后台 resume 取消后的 `Working` 残留

确认证据（基线 `a7442924`）：

- 后台 resume 在 `peri-agent/src/session/subagent/background.rs:744-760` 先写
  `WorkState::Working`，再 `confirm_background_start` 确认启动。`set_work_state`
  可能在「事务已提交、调用尚未返回」时被调用方取消；调用方 future 被 drop 后
  `handoff` 一起 drop，认领 worker 的 `accepted.await` 失败。
- 认领 worker 的未接收补偿只调用 `ChildResumeClaim::mark_failed()`
  （`peri-agent/src/session/subagent/factory/claim.rs:233-238`），它只恢复
  `agent_status`，不恢复 `work_state`。
- 故障注入实测（修复前）：用例 `cancel_after_working_commit_restores_work_state`
  在 `abort()` 后稳定读到
  `ThreadMeta { agent_status: Done, work_state: Working, ... }`；调用方在启动确认
  前被取消，后台从未放行。
- 同一残留形态，与主问题的「终态 + `working`」同族：下一次进入会话会把已停止的
  child 当成待续跑。

修复：把取消补偿交给已存在的独立 claim worker——调用方消失后仍由它收尾。worker
在移交前记住认领前的 `work_state`，未接收时在 `mark_failed()` 之后恢复它；
`background.rs` 的同步补偿保留为同一值的快速路径，不依赖 worker 的异步时序。
Working 写入仍留在 `background.rs`（成功返回前必须已落库），不迁移到 worker。

## 影响面

- 用户影响：已有终态的 child 在父会话 load/resume 时被自动续跑，产生额外模型
  调用与重复结果通知；`Agent(resume_thread_id: ...)` 之外的隐式执行。后台 resume
  被取消时留下「终态 + `working`」，会放大同一后果。
- 受影响模块：`peri-acp` 恢复协调、`peri-resources` SQLite MQ 结算与工作意图
  维护、`peri-agent` resume 认领收尾与后台移交。
- 可规避：只能手动避免 load/resume 含该 child 的父会话；无配置级开关。

## 修复方案

- 范围（无 schema 变更）：
  1. 恢复复核加终态门：`prepare_root` / `prepare_child` 读到 `agent_status`
     为终态时按 `Idle` 跳过，不注入提醒、不装配执行；保留
     `Active + Working` 的崩溃续跑语义。
  2. 终态 + `working` 收敛：`finalize_child_terminal_mq` 与
     `settle_terminal_without_result_rows` 在 `current.is_terminal()` 时于同一
     事务内把 `work_state` 收敛为 `WorkState::after_agent_terminal(current)`；
     不改写 `agent_status`、不新增 MQ 行。
  3. 禁止终态提升：`maintain_work_intent_on_accept` 与远端同名提升语句
     （`PROMOTE_WORK_INTENT_SQL`）只对 `agent_status = 'active'` 的 thread 执行
     `idle → working`；显式 resume 的认领写入不受影响。
  4. 后台 resume 取消补偿：认领 worker 未接收移交时，在 `mark_failed()` 之后把
     `work_state` 恢复到认领前的值，消除「终态 + `working`」的取消窗口。
- 明确不做：
  - close 写终态（`close` 成功目前只删 close intent，`lifecycle.rs:6-28`）。
    是否让 close 把 root 子树收敛为终态需要产品裁决；在裁决前，已关闭的
    `Active + working` child 仍会被恢复。用例 `closed_child_is_not_recovered`
    以 `#[ignore]` 记录该期望。
  - `ClaimDecision::Finish` / `CancelRunning` 的原子终态结算：现有
    `settle_terminal_without_result` 要求当前 `active`，而 `mark_terminated`
    已把记录写回认领前的终态，直接复用会把新终态吞成 no-op。两条最小路径：
    （A）新增「从认领收尾写终态」的资源接口（trait + SQLite + 远端 + 测试替身），
    一次提交 `agent_status` 与派生 `work_state`；（B）改
    `ChildResumeClaimHandle::mark_terminated` 不再回写 previous、保留 `active`，
    复用现有 `settle_terminal_without_result`。选 B 会失去「终态写入失败也不留
    `active` 残留」的现有保证；A 需要新增领域接口并说明认领所有权语义。两者
    都超出本次手术范围，待裁决。
  - 通知文案、TUI 投影、TaskManager 登记、`interrupted` 收敛与
    `Stopped` 不自动续跑语义。
  - 远端 MQ 迟到结算的 `working` 收敛：`remote/session_mq.rs:259-261`、
    `remote/session_settlement.rs:147-150` 仍只返回既有终态；恢复门与后端无关，
    残留值不再触发续跑，但仍是脏元数据。`PROMOTE_RECEIVER_*` 仍不校验接收方终态。
  - 真实进程崩溃注入：本次只用 `Active + Working` 用例覆盖崩溃续跑语义，
    未运行 kill -9 或断点注错。
- 回归测试：
  - `peri-acp/src/host/recovery_child_test.rs`：
    `terminal_child_with_residual_working_is_not_recovered`（`Done` /
    `Cancelled` / `Error` + `working`：`recovered` 为空、无提醒、模型零调用）、
    `closed_child_is_not_recovered`（`#[ignore]`，记录 close 语义缺口）。
  - `peri-resources/src/sessions/sqlite_store/session_mq_terminal_test.rs`：
    `late_finalize_converges_terminal_child_residual_working`、
    `late_settle_without_result_converges_terminal_child_residual_working`。
  - `peri-resources/src/sessions/resources_mq_test.rs`：
    `mq_acceptance_does_not_promote_terminal_thread`。
  - `peri-resources/src/sessions/remote/session_mq_test.rs`：
    `remote_acceptance_does_not_promote_terminal_thread`。
  - `peri-agent/src/session/subagent/factory/background_handoff_test.rs`：
    `cancel_after_working_commit_restores_work_state`（Working 已提交后取消：
    `agent_status` 回到 `Done`、`work_state` 回到认领前的 `Stopped`，后台零启动）。

## 验收标准

1. 父会话 load/resume 时，`Done|Cancelled|Error + working` 的 child 不出现在
   `recovered`，不产生 recovery reminder，不调用模型；`Active + Working` 仍按
   崩溃续跑。
2. 终态 child 的迟到结算只把残留 `working` 收敛为派生工作意图
   （`Cancelled → Stopped`，其余 `Idle`），终态与结果行不变。
3. 终态 thread 的 `EnsureProcessing` 接纳不改变 `work_state`。
4. 后台 resume 在 Working 已提交后被取消：`agent_status` 回到认领前终态，
   `work_state` 回到认领前值，后台零启动。
5. `cargo fmt --all -- --check`、`clippy --all-targets -- -D warnings` 与目标
   crate 的 `--lib` 测试通过。

## 验证记录

独立 verifier 复核（基线 `a7442924` + 本分支改动）：

1. 改动范围：代码修复 13 个文件（2 个文档 + 11 个代码/测试），无 schema 变更；
   收尾另同步 `docs/code-index/peri-acp.md:81` 的终态门描述。
2. 代码事实：
   - `effective_work_state()`（`peri-acp/src/host/recovery/coordinator.rs:95`）
     把 `Done`/`Cancelled`/`Error` 映射为 `Idle`；`prepare_root`、`prepare_child`
     与二次复核都经它判定（同文件 `:247`、`:293`、`:323`）。
   - 本地 `finalize_child_terminal_mq` / `settle_terminal_without_result_rows`
     在事务内调用 `converge_terminal_work_state`
     （`peri-resources/src/sessions/sqlite_store/session_data/mq.rs:642`、`:725`、
     `:754`、`:947`）：终态已落时只收敛 `working`，不改写 `agent_status`、
     不新增 MQ 行。
   - 本地与远端 MQ 接纳提升 SQL 均加 `agent_status = 'active'`
     （同文件 `:246`；`peri-resources/src/sessions/remote/session_mq.rs:54`）。
   - 认领 worker 在移交前记录 `previous_work_state`，未接收补偿时在
     `mark_failed()` 后恢复它
     （`peri-agent/src/session/subagent/factory/claim.rs:221`、`:243`）。
3. 测试实测（独立 verifier）：
   - 本地 session_mq 套件 → 68 passed
   - resources mq 套件 → 7 passed
   - peri-agent subagent 套件 → 259 passed
   - `cancel_after_working_commit_restores_work_state` → 1 passed
   - peri-acp recovery 套件 → 99 passed / 1 ignored
   - `recovery_child::terminal_` → 2 passed
   - `Active + Working` 续跑用例 → 2 passed
   - 全量 `--lib`：peri-acp 1234 passed / 2 ignored；peri-agent
     1510 passed / 5 ignored；peri-resources 731 passed / 34 ignored；0 failed
   - `cargo fmt --all -- --check`、`clippy --all-targets -- -D warnings`、
     `git diff --check`、workspace build 均通过。
4. 已知缺口 / 残余：
   - 远端 MQ 迟到结算未收敛 `working`
     （`peri-resources/src/sessions/remote/session_mq.rs:259-261`、
     `peri-resources/src/sessions/remote/session_settlement.rs:147-150`）；
     恢复门与后端无关，残留值不再触发续跑，但仍是脏元数据。
   - `ClaimDecision::Finish` / `CancelRunning` 仍只写 `agent_status`，
     依赖后续结算收敛残留 `working`，不是原子收尾。
   - close 不写终态；`closed_child_is_not_recovered` 仍为 `#[ignore]`，
     实测 FAILED（记录预期缺口，未改成通过）。
   - `PROMOTE_RECEIVER_*` 不校验接收方终态，收到终态 thread 的结果时仍可能
     提升其 `idle → working`。
   - 真实进程崩溃未注入；崩溃续跑语义仅由 `Active + Working` 用例覆盖。
