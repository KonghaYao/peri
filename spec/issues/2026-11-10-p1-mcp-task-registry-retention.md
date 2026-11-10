# P1：MCP task registry 累积保留已完成订阅

**状态**：已修复，待 PR/CI 验收。
**优先级**：P1。
**类型**：性能（内存保留）。
**创建日期**：2026-11-10。
**来源**：用户提供 4.7.0 报告《Peri 4.7.0 MCP 任务累积保留 P1 问题与修复方案》+ coder 复核。
**确认方式**：coder 在隔离 worktree 复现 30k 唯一 task id 用例，并沿 `McpTaskOwner → state.records → TaskRecord` 复核代码路径（基线 `a7442924`，worktree `fix/mcp-task-registry-retention`）。

## 确认证据

1. 修复前 `review_monitor_memory_completed_subscriptions_release_records` 的输出：

   ```text
   MEMORY completed=1000  retained_keys=1000  retained_records=1000
   MEMORY completed=10000 retained_keys=10000 retained_records=10000
   MEMORY completed=30000 retained_keys=30000 retained_records=30000
   assertion `left == right` failed: naturally completed unique task subscriptions must release owner records
     left: 30000
    right: 0
   test result: FAILED. 0 passed; 1 failed
   ```

2. `CompletionGuard::drop` 只把匹配记录置为 `TaskPhase::Finished`，不摘除记录（`peri-middlewares/src/mcp/task_scope.rs:143`，基线 `:144`）。
3. `spawn` 只清理本次传入 key 的 Finished 记录；`stop_key` 等待完成后也只清理同一 key（基线 `:254`、`:299`）。不同 `TaskStatus { server, task_id }` 的完成记录因此永久保留。
4. 保留链为 `McpTaskOwner → McpTaskInner.state.records → TaskRecord`；记录持有键内字符串、`AbortHandle`、`Arc<Completion>` 与容器分配。`shutdown()` 是唯一能清空全部记录的路径，正常运行时不会触发。

## 根因链

1. 每个 MCP task 以唯一 `TaskStatus { server, task_id }` 登记，任务自然结束后 guard 只标记 Finished。
2. Finished 记录不参与 `spawn` 的同 key 判重以外的回收；后续 task id 不同，旧 key 永远不会被再次访问。
3. 因此登记记录数与历史任务总数同步增长；长期运行宿主的内存由历史任务数决定，而不是在途任务数。
4. 观察：记录数增长与任务数线性相关；推断：真实 monitor 反复启动/停止时同样保留每轮的键、控制句柄和完成对象。未观察到单条事件正文随记录保留。

## 影响面

- 用户影响：长期运行、反复创建不同 id 的 MCP task（monitor 重启、停止后另建监控）时，宿主登记内存持续增长；固定 key 的清理路径无法覆盖。
- 受影响模块/调用点：
  - `peri-middlewares/src/mcp/task_scope.rs`：owner 登记表、guard、spawn、stop_key、abort_all。
  - `peri-middlewares/src/mcp/client/subscription_tasks.rs:97`：每个终态订阅以唯一 task id 登记。
  - `peri-middlewares/src/mcp/client/lifecycle.rs:374`：`stop_key` 调用方。
- 是否可规避：仅能通过整体 `shutdown` 释放；不能靠固定 key 复用规避。

## 修复方案

- 范围：
  1. `CompletionGuard::drop` 在锁内只摘除同时匹配 `key + generation` 的记录，向量为空时移除 key；释放锁后再 drop 摘出的记录，最后 `completion.finish()`。完成信号晚于登记摘除，等待方观察到完成时不会看到同代残留。
  2. 删除 `TaskPhase::Finished`、`compact_finished` 与同 key 的去重清理；登记表仅保留 Running / Stopping 的在途记录。
  3. `spawn` 语义不变：owner 开放且 key 无在途记录时登记新 generation；重复 key 返回 `DuplicateKey`，关闭后返回 `OwnerClosed`。
  4. `stop_key` 只在锁内置 Stopping 并取得控制对象，锁外 abort 与等待；不再在等待后按 key 整体删除（避免误删同名新 generation）。
  5. `shutdown` 保留关闭准入、abort、`TaskTracker` 等待与末尾 `records.clear()` 兜底；owner Drop、首次 poll 前取消、panic 展开均由同一 guard 摘除。
  6. 容器容量允许复用峰值，不要求 `shrink_to_fit`；不新增表结构、持久化字段、MCP 协议或 monitor 预算变更。
- 明确不做：不改变 monitor 事件缓冲策略；不引入定期重启/强制回收；不修改 `TaskStatus` 协议语义；不为 poison 增加额外恢复分支。
- 回归测试：
  - `peri-middlewares/src/mcp/task_scope_test.rs`：`review_monitor_memory_completed_subscriptions_release_records`、`completed_task_releases_future_captures`、`completed_key_can_be_reused_after_registration_release`、`natural_completion_and_stop_key_release_the_same_record`、`canceled_stop_key_waiter_still_releases_the_task`、`panicking_task_releases_registration`、`closing_owner_drains_inflight_registration_and_rejects_new_tasks`、`completed_rounds_reuse_registry_capacity`、`stale_generation_guard_keeps_newer_registration`。
  - `peri-middlewares/src/mcp/client/monitor_subscription_test.rs`：`completed_and_cancelled_monitor_rounds_release_host_registrations`。

## 验收标准

1. 30k 个唯一 task id 自然结束后，owner 未关闭时 `records.len() == 0`、记录总数为 0。
2. 连续多轮完成/取消同一 key 的任务，终态无残留；固定峰值并发下容器容量不随历史轮数增长。
3. future 捕获对象、完成对象与句柄按预期释放；完成信号仅在登记摘除后可见。
4. 自然完成、panic、运行中取消、首次 poll 前取消、取消 stop_key 等待方、关闭竞态均通过。
5. 旧 generation 的 guard 不删除、不停止同 key 新 generation。
6. 重复完成/取消 monitor 任务后，宿主登记回到基线，终态提醒每轮恰好一条。
7. monitor 事件容量、共享预算与过期清理测试继续通过。
8. `cargo fmt --all -- --check`、`clippy --all-targets -- -D warnings`、`git diff --check` 通过。

## 遗留风险与未覆盖

1. Linux RSS 未采样：宿主机为 macOS，无 `/proc/self/status`；内存回收以登记归零和对象释放断言为证据，未做 Linux 堆趋势采样。
2. `abort_all` 的 Stopping 细分（只中止 Running）无独立断言，仅由现有停止/关闭用例间接覆盖。
3. 未跑全量 `peri-middlewares` 测试，只覆盖 task_scope、monitor_subscription 与 subscription::tasks 相关用例。
4. mutex poison 语义未改：状态锁仍按 poison 直接 panic，未新增恢复分支。

## 验证记录

- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib review_monitor_memory_completed_subscriptions_release_records -- --nocapture`（修复前）→ FAILED：`retained_keys=30000`、`retained_records=30000`，断言 `left: 30000, right: 0`。
- 同一命令（修复后）→ PASSED；1000 / 10000 / 30000 三个阶段均为 `retained_keys=0 retained_records=0`。宿主机为 macOS，无 `/proc/self/status`，`rss_kib=0` 不参与判断；硬断言为登记归零。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib task_scope` → ok，16 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib monitor_subscription` → ok，9 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib 'mcp::client::subscription::tasks::'` → ok，30 passed，0 failed。
- `./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-monitor --lib` → ok，55 passed，0 failed。
- `cargo fmt --all -- --check` → exit 0。
- `./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-middlewares --all-targets -- -D warnings` → exit 0。
- `git diff --check` → exit 0。
- 以上测试与检查在提交前于 worktree `fix/mcp-task-registry-retention`（基线 `a7442924`）重新实跑，结果一致。
