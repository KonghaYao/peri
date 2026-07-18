# Compact 效果在 v2 路径中跨 prompt 丢失，上下文使用率每轮重置到 100%

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-18

## 问题描述

在长对话中，Micro Compact / Full Compact 执行后 StatusBar 显示的上下文使用率从高位（如 80%）降到合理值（如 40%）。但紧接着发下一个 prompt 后，上下文使用率立即重新跳到 100%。compact 的效果仅在当前 turn 内有效，无法延续到下一轮对话。

注意：这不是 session 停止再 resume 的场景——就是**同一 session 内连续对话**，上一轮 compact 了，下一轮上下文又满了。

相关 issue：`spec/issues/2026-07-17-compact-flags-lost-on-session-restore.md`（描述 session resume 场景下 DB 加载路径的问题——二者是 compact 效果丢失的不同环节，本 issue 描述的丢失点更前置）。

## 症状详情

| 时刻 | StatusBar 上下文 | 说明 |
|------|-----------------|------|
| 第 N 轮，compact 触发前 | ~80% | 长对话积累，上下文使用率升高 |
| 第 N 轮，compact 执行后 | ~40% | compact 截断旧消息，input_tokens 下降 |
| 第 N 轮，LLM 回复完成 | ~40% | 看起来正常 |
| 第 N+1 轮，发 prompt 后 | 立即 100% | compact 效果完全丢失，LLM 收到完整消息 |

关键症状：**连续两轮 prompt 之间** compact 效果就消失了。不需要停止对话、不需要 resume——第二轮 prompt 发出后上下文就回到 100%。

## 丢失链路（经过对抗审查修正）

```
Turn N: compact 执行
  → transcript.set_truncated/set_excluded      [内存 flags HashMap 写入 ✓]
  → send_persist(PersistOp::UpdateFlags)       [✗ NO-OP: persist_tx 为 None]
  → 同 turn 内 Reason 读取 transcript flags   [✓ turn 内 compact 有效]
  → Turn 结束，V2Session 销毁，flags HashMap 随 transcript drop

Turn N+1: 新 prompt
  → 新 V2Session（persist_tx 仍为 None）
  → Phase 5: append_batch(history)            [history 来自 state.history，不含 flags]
  → Phase 5.5: store.load_message_flags(tid)  [代码结构正确，但 DB 中无 flags——上轮未写入]
  → flags HashMap 初始为空 → LLM 收到完整消息 → 上下文 100%
```

### 关键事实澄清

1. **Phase 5.5 不是死代码**——它可以执行且结构正确（`executor_helpers.rs:683-706`），`load_message_flags` trait + `SqliteThreadStore` 实现 + `set_flags_batch` 均已就位。阻塞点在上游：compact 阶段的 flag 写入通过 `send_persist` 通道，该通道在 v2 主路径中 `persist_tx` 始终为 None。

2. **turn 内 compact 确实有效**——`run_react_loop` 中 Compact → Reason 在同一迭代内共享 transcript，Reason 能正确读取 compact 设置的 flags。

3. **prompt.rs 持久化路径已评估，无额外影响**（详见下方「prompt.rs 路径影响评估」节）。

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-acp/src/agent/builder_v2.rs:139-145` | v2 Session 创建时 `thread_id` 传 `None`，`MessageTranscript` 的 `persist_tx` 始终为 None |
| `peri-agent/src/session/transcript.rs:477-483` | `send_persist` 在 `persist_tx.is_none()` 时直接 return——所有 flag 写入均为 no-op |
| `peri-agent/src/session/transcript.rs:147-170` | `with_persistence()` 已实现，但 v2 主路径从未调用 |
| `peri-acp/src/session/executor_helpers.rs:586-588` | `thread_store`/`parent_thread_id` 已从 cfg 提取，但未传给 `build_stage_context` |
| `peri-acp/src/session/executor_helpers.rs:683-706` | Phase 5.5 flag 恢复逻辑已就位，等上游数据 |
| `peri-tui/src/acp_server/prompt.rs:203-206` | `append_messages()` 只写消息不写 flags——第二个丢失点 |
| `peri-tui/src/acp_server/prompt.rs:232` | `state.history = result.messages`——`BaseMessage` 不携带 flags |

## 修复方向

**最小必要改动**（下游基础设施均已就位）：

1. `build_stage_context` 接收 `thread_store`/`thread_id` 参数，传给 V2Session 创建
2. V2Session 创建后调用 `transcript.with_persistence(store, thread_id)` 激活持久化通道
3. 调用方 `build_and_execute_agent_v2` 传递已有的 `thread_store`/`parent_thread_id`

修复后链路：
```
compact → set_truncated → send_persist(UpdateFlags) → DB 写入 ✓
Turn N+1 → Phase 5.5: load_message_flags → set_flags_batch → transcript 恢复 flags ✓
```

## prompt.rs 持久化路径影响评估

`prompt.rs:201-206` 在每轮结束后调用 `thread_store.append_messages(&thread_id, new_msgs)`，但 `BaseMessage` 结构体不携带 `truncated`/`excluded` 字段。**此路径是否构成 flag 丢失的第二个来源？**

### 分析

| 场景 | prompt.rs 行为 | flag 影响 | 结论 |
|------|---------------|----------|------|
| 正常追加（`result.messages > history`） | 只写新消息（`history_len..` 的 delta） | 新消息从未有过 flags，无需恢复 | ✅ 安全 |
| Micro compact（消息数接近，截断不删除） | 走正常追加路径 | 被截断的消息已存在 DB 中，`INSERT OR IGNORE` 保留原行（含 flags） | ✅ 安全 |
| Full compact（消息数减少，旧消息被替换为摘要） | `delete_messages` + `append_messages` | 旧消息被删除，摘要消息是新的——旧 flags 随消息删除而失效，正确行为 | ✅ 正确 |

关键保护机制：
1. `sqlite_store.rs:322:` **`INSERT OR IGNORE`** — 已存在的 message_id 不会被覆盖，flag 列保持不变
2. `load_message_flags`（`sqlite_store.rs:671`）独立查询 `truncated, excluded` 列，不与 content 耦合
3. Full compact 路径（`prompt.rs:206-231`）删除旧消息 + 写入新摘要——旧 flags 随消息一起删除，语义正确

**结论：prompt.rs 路径无新增风险，无需额外修复。** 修复后 transcript writer task 承担消息和 flags 的全部持久化，prompt.rs 的写入变为冗余但幂等的操作（`INSERT OR IGNORE`），不干扰 flag 状态。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-18 | — | Open | agent | 创建 |

## 修复记录

### 修复 #1（2026-07-18）

- **操作人**：agent（devflow: explore → plan → code → review → verify）
- **用户原意**：修复 v2 路径中 compact 效果跨 prompt 丢失，使 compact flags 可跨 turn 持久化
- **修复内容**：
  - `peri-acp/src/agent/builder_v2.rs`（+12 行）：`build_stage_context` 签名新增 `thread_store`/`thread_id` 参数，V2Session 创建后条件性调用 `transcript.with_persistence()` 激活持久化通道
  - `peri-acp/src/session/executor_helpers.rs`（+2 行）：调用处传递已提取的 `thread_store`/`parent_thread_id`
- **下游不变**：Phase 5.5（`load_message_flags` → `set_flags_batch`）、`SqliteThreadStore::load_message_flags`、`send_persist(PersistOp::UpdateFlags)` 等基础设施均未改动
- **验证状态**：`cargo check` + `cargo test` 全通过（peri-acp: 321 tests, peri-agent: 10 tests session::tests），无级联影响
- **Handoff**：`.tmp/devflow/compact-v2-persistence/`
- **待手动验证**：实际运行长对话，compact 后发新 prompt，确认上下文使用率不跳回 100%
