# P1：MutationGate 的 pending 条目只插入不回收

**状态**：已修复，待 PR/CI 验收。
**优先级**：P1。
**类型**：错误（资源泄漏 / 会话生命周期）。
**创建日期**：2026-11-10。
**来源**：4.7.1 复核证据包 + 探针复现。
**确认方式**：cargo 探针在基线 `4ba78d7e` 实测 300 删除后残留 300；主 agent
复核代码路径。

## 确认证据

- 探针 `review_closed_deleted_sessions_release_mutation_gate_entries`
  （`peri-resources`）失败：`left: 300, right: 0`；测试打印
  `deleted_roots=300 pending_entries=300 live_barriers=300`。位置
  `peri-resources/src/sessions/resources_lifecycle_test.rs:28`。
- 只插入不删除：`peri-resources/src/sessions/resources/gate.rs:123-130`
  `pending_for` 使用 `self.pending.lock().entry(root.clone()).or_default()`
  —— 新 root 即插入，返回 `Arc<PendingWrites>`。
- `pending: Arc<Mutex<HashMap<ThreadId, Arc<PendingWrites>>>>`
  （`gate.rs:63-66`）；全文件没有任何 `remove`/`retain` 回收路径。
- `PendingWrites` 持有一个 `Arc<RwLock<()>>` 屏障
  （`gate.rs:26-29`），删除会话后仍保持存活引用。

## 根因链

1. 每次 mutation 对 root 调用 `pending_for`，首次访问插入空
   `PendingWrites`（观察：`gate.rs:123-130`）。
2. `admit` / `scope` 复用该条目，但 close/delete 成功路径不清理
   （观察：全文件无回收）。
3. 会话删除后 `PendingWrites` 与 `RwLock` 仍被 map 强引用；探针中 300 个
   删除 root 对应 300 个 pending entry 与 300 个活屏障（观察）。
4. 长期进程内删除/创建会话使 map 单调增长；空屏障也可能令后续诊断误判
   "仍有写入未结清"（推断）。

## 影响面

- 用户影响：长驻进程批量删除会话后内存不释放；关闭/删除的会话仍保留
  写入门禁状态，可能扩大重启恢复与诊断的噪声。
- 受影响模块：`peri-resources` 会话资源写入门禁与生命周期。
- 可规避性：重启进程。

## 修复方案

- 建立统一获取/回收协议：回收与获取在同一 `pending` map 锁内串行化。
- 回收条件必须同时满足：
  - 该 root 的会话已关闭/删除完成；
  - `Arc::strong_count` 表明只有 map 持有（或使用 `Arc::ptr_eq` 确认移除
    的正是当前条目）；
  - `PendingWrites.uncertain == false`（未知结果不得丢弃）。
- 在 close/delete 成功路径触发回收；不确定写保留，交由既有恢复流程。
- 明确不做：
  - 不引入定时清理线程或全局 LRU 掩盖生命周期问题；
  - 不改变 `WriteScope` 的 settle/uncertain 语义；
  - 不在持有锁时 await；
  - 不做 schema 变更。
- 回归测试（探针转正向）：
  - 删除 N 个 root 后 pending 条目归零；
  - `uncertain=true` 的条目不被回收；
  - 并发 admit 与回收不误删仍在使用中的条目（`Arc` 身份保护）。

## 验证记录

- 回归测试（`peri-resources`）：`closed_deleted_sessions_release_mutation_gate_entries`、
  `uncertain_pending_entries_are_not_released`、
  `uncertain_pending_entry_survives_delete_path`、
  `pending_entries_in_use_are_not_released` 均通过。
- `peri-resources --lib` 全量：738 passed，0 failed，34 ignored。
- `./scripts/check-changed.sh` 与 `cargo fmt --check` 通过。
- 主 agent 复核：4 项回归测试实跑通过。
- 已知边界：仅当被关闭/删除的 id 就是 root 时回收；child 子树删除不回收 root 条目。
