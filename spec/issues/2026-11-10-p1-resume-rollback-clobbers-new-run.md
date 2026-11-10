# P1：旧 resume 补偿无条件回滚工作意图，覆盖已开始的新任务

**状态**：已修复，待 PR/CI 验收。
**优先级**：P1。
**类型**：错误（回归 / 恢复正确性）。
**创建日期**：2026-11-10。
**来源**：4.7.1 复核证据包（#382 引入）+ 探针复现。
**确认方式**：cargo 探针在基线 `4ba78d7e` 实际复现（factory 与 SQLite 两条路径）；
主 agent 复核代码路径。

## 确认证据

- 探针 `review_cancelled_resume_rollback_cannot_stop_new_resume`
  （`peri-agent`）失败：`left: Stopped, right: Working`，位置
  `peri-agent/src/session/subagent/factory/background_handoff_test.rs:889`。
  前置断言均通过：新 run 已 `Active + Working`、`requests=1`、
  `active_count=1`。
- 探针 `review_sqlite_late_resume_rollback_preserves_new_working`
  （`peri-resources`）失败：`left: Stopped, right: Working`，位置
  `peri-resources/src/sessions/resources_child_test.rs:36`；输出包含
  `ROLLBACK sqlite_new_status=Active sqlite_new_work_state=Stopped`。
- 无条件补偿点一：`peri-agent/src/session/subagent/factory/claim.rs:240-243`
  —— HandOff 未被接受时
  `rollback_handoff(handle.mark_failed(), store.set_work_state(&thread_id, previous_work_state))`
  直接写回认领前的 `previous_work_state`。
- 无条件补偿点二：`peri-agent/src/session/subagent/background.rs:769-789`
  —— 确认失败时再次同步 `set_work_state(previous)`。
- `previous_work_state` 来自旧认领的 `meta.work_state`
  （`claim.rs:96-97`），可能是 `Stopped`；写入前不核对当前值是否仍属于
  本次认领。

## 根因链

1. 旧 resume 认领 child，记下 `previous_work_state`（可能是 `Stopped`）并
   写入 `Active`（观察：`claim.rs:acquire`）。
2. 调用方消失或移交未被接受，进入补偿分支（观察：`claim.rs:236-244`）。
3. 同时新 resume/新任务已开始并把状态写成 `Active + Working`
   （观察：探针前置断言）。
4. 旧补偿无条件 `set_work_state(previous_work_state)`，把新任务的
   `Working` 覆盖回 `Stopped`（观察：探针 `left: Stopped`）。
5. 恢复路径看到 `Active + Stopped`，判定任务已终止，跳过新任务
   （推断：由 `effective_work_state` 语义决定）。
6. `background.rs` 的第二次独立写入与 `claim.rs` 的补偿叠加，缺少统一
   所有者与原子性（观察：两处独立 `set_work_state`）。

## 影响面

- 用户影响：取消/失败并发下，新 resume 被旧补偿标记为已停止，后台任务
  不再继续或结果不被回收。
- 受影响模块：`peri-agent` 子代理认领/后台移交、`peri-resources` 会话
  资源写入门禁。
- 可规避性：无；取决于补偿与新任务的时序。

## 修复方案

- 归属身份不能用持久记录值表达：新 run 的正常状态 `Active + claimed + Working`
  与旧认领写入的中间态完全相同。资源层维护进程内认领世代
  （`MutationGate::ClaimLedger`）：`claim_child_resume` 在写入门禁内分配世代，
  `settle` 在同一门禁内先校验世代仍是本次认领，校验失败则放弃补偿并记录
  `warn!`（`thread_id`、世代），新认领一旦分配旧世代立即失效。
- 状态列与工作意图列的恢复仍在同一资源门禁内完成，只有归属校验通过才写两列，
  避免 `mark_failed` 与 `set_work_state` 之间被新任务插入。
- 由 `ChildResumeClaim` 成为唯一补偿所有者；`background.rs` 不再独立写
  `set_work_state`，接管失败只返回错误，由 worker 统一收尾。
- 明确不做：
  - 不改变正常 HandOff/Finish/Release 的提交顺序；
  - 不加数据库字段或迁移；
  - 不引入重试循环掩盖竞态；
  - 不扩大 child 恢复语义。
- 已知限制：认领世代是进程内状态。跨进程、或同一数据库上另一个门面实例的
  写入无法与本次认领区分；远端 store 仍依赖其自身的原子写与 `active` 守卫。
- 回归测试（探针转正向）：
  - 旧补偿晚于新任务 Working 到达 → 新任务保持 `Active + Working`；
  - 旧补偿先到 → 正常恢复 `previous_work_state`；
  - 两列中只有一列被新任务改写时，不得部分回滚。

## 验证记录

- 失败基线（临时禁用世代校验，仅用于证明测试有效）：
  - `peri-resources`：`late_resume_rollback_preserves_new_working` 失败，
    `left: Done, right: Active`（`resources_child_test.rs:78`）。
  - `peri-agent`：`stale_resume_rollback_preserves_new_working_run` 失败，
    `left: Done, right: Active`（`background_handoff_test.rs:897`）。
- 恢复世代校验后：
  - `cargo test -p peri-resources --lib -- late_resume_rollback_preserves_new_working rollback_skips_both_columns`：3 passed。
  - `cargo test -p peri-agent --lib -- stale_resume_rollback_preserves_new_working_run cancel_after_working_commit_restores_work_state`：2 passed。
  - `cargo test -p peri-resources --lib`：738 passed，0 failed，34 ignored。
  - `cargo test -p peri-agent --lib`：1515 passed，0 failed，5 ignored。
  - `./scripts/check-changed.sh`：通过。
  - `cargo fmt --check`：通过。
- 测试时序：旧认领移交后台并写入终态后，补偿结清被 `WorkRollbackGate` 卡在
  入口；新 run 完成认领、提交 `Working` 并进入模型；随后释放旧补偿。按记录值
  判断归属的实现会回滚成 `Done + Stopped`，世代校验会识别新认领并放弃。
