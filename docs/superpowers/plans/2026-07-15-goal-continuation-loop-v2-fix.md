# Goal 自驱续跑 v2 修复计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 goal 模块在 v2 架构下的自驱续跑——agent 调用 `goal(create)` 后能自动跨 turn 持续执行，直到 `goal(complete)` / `goal(block)` 终止。

**Architecture:** 核心修复只需一行——`GoalMiddleware::after_agent` 中将 goal steering 注入由 `MessageKind::Info` 改为 `MessageKind::Defer`。Defer 在 End 阶段触发 `should_continue=true`，循环自驱续跑；complete/block 后 goal 进入终态，不再 push Defer，循环自然退出。

**Tech Stack:** Rust, peri-middlewares, peri-agent (MessageQueue/Defer/End stage)

---

## 诊断回顾

issue: `spec/issues/2026-07-15-goal-continuation-loop-broken-in-v2.md`

v1→v2 迁移时 goal 的续跑机制有两条路径同时断裂：

| 断裂 | 位置 | 机制 |
|------|------|------|
| `block_continue` 被丢弃 | `act.rs:135` | `AgactOutput` 只提取 `.text`，`block_continue` 静默丢弃 |
| steering 用 `Info` 注入 | `goal_middleware.rs:111` | `MessageKind::Info` 被 End 阶段保留不消费，`should_continue` 永远为 false |

**修复策略**：走 Defer 路径（而非恢复 block_continue）。原因：
- Defer 在 End 阶段被 `drain_for_end` 消费并写入 transcript → `should_continue=true` → 原生续跑
- 无需改动 Act/ActOutput/run_react_loop 的控制流
- block_continue 是 v1 executor 的机制，在 v2 单路径中已被 Defer + End 替代

**流程序列**（修复后）：
```
goal(create) → Active
  after_agent: push Defer(GoalSteering)
  End: drain_for_end 找到 Defer → should_continue=true → 写 transcript → continue loop
下一 turn: Reason 看到 steering → agent 继续工作
  after_agent: 仍 Active → 再 push Defer
  End: continue loop
...
goal(complete) → Complete
  after_agent: !is_goal_active → 不 push Defer
  End: 无 Defer → should_continue=false → LoopResult::Completed → 自然退出
```

---

### Task 1: 修改 steering 注入类型

**Files:**
- Modify: `peri-middlewares/src/goal_middleware.rs:111`

- [ ] **Step 1: 将 MessageKind::Info 改为 MessageKind::Defer**

在 `peri-middlewares/src/goal_middleware.rs` 第 111 行，修改 `MessageKind::Info` → `MessageKind::Defer`：

```rust
// Before (line 109-114):
let reminder = format!("<system-reminder>\n{}\n</system-reminder>", template);
state.v2_queue().push(QueuedMessage::new(
    MessageKind::Info,          // ← 改这里
    MessageSource::GoalSteering,
    BaseMessage::human(MessageContent::text(reminder)),
));

// After:
let reminder = format!("<system-reminder>\n{}\n</system-reminder>", template);
state.v2_queue().push(QueuedMessage::new(
    MessageKind::Defer,
    MessageSource::GoalSteering,
    BaseMessage::human(MessageContent::text(reminder)),
));
```

- [ ] **Step 2: 编译验证**

```bash
cargo build -p peri-middlewares 2>&1
```
Expected: 编译通过，无错误。

- [ ] **Step 3: Commit**

```bash
git add peri-middlewares/src/goal_middleware.rs
git commit -m "fix(goal): use MessageKind::Defer for steering to enable v2 continuation loop

MessageKind::Info 在 drain_for_end 中被保留不消费，should_continue 永远为 false。
改为 Defer 后 End 阶段发现 Defer 即唤醒新 turn，达成 goal 自驱续跑。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 2: 更新单元测试

**Files:**
- Modify: `peri-middlewares/src/goal_middleware_test.rs:75-93`
- Modify: `peri-middlewares/src/goal_middleware_test.rs:146-201`

- [ ] **Step 1: 更新 `test_after_agent_goal_active_注入_steering_并设_block_continue`**

第 90-92 行，`drain_for_receive` 不会消费 Defer，改用 `drain_for_end`：

```rust
// Before (line 90-92):
// 注入路径：v2 MessageQueue 应收到 1 条 Info（GoalSteering）
let drained = state.v2_queue().drain_for_receive();
assert_eq!(drained.len(), 1, "应 push 1 条 goal steering Info 消息");

// After:
// 注入路径：v2 MessageQueue 应收到 1 条 Defer（GoalSteering）
// Defer 在 Receive 阶段保留在队列，需用 drain_for_end 验证
let drained = state.v2_queue().drain_for_end();
assert_eq!(drained.unwrap_or_default().len(), 1, "应 push 1 条 goal steering Defer 消息");
```

同时更新第 90 行注释中 "Info（GoalSteering）" → "Defer（GoalSteering）"。

- [ ] **Step 2: 更新 `test_after_agent_terminal_重置_pending_rounds`**

第 194-200 行，同样改 `drain_for_receive` → `drain_for_end`：

```rust
// Before (line 194-200):
// 验证 queue 累积：两次 active（r1 / r3）各 push 1 条 Info，r2 终态不 push
let drained = state.v2_queue().drain_for_receive();
assert_eq!(
    drained.len(),
    2,
    "应累积 2 条 goal steering（两次 active），终态 r2 不 push"
);

// After:
// 验证 queue 累积：两次 active（r1 / r3）各 push 1 条 Defer，r2 终态不 push
// Defer 在 Receive 阶段保留在队列，需用 drain_for_end 验证
let drained_msg = state.v2_queue().drain_for_end();
assert_eq!(
    drained_msg.unwrap_or_default().len(),
    2,
    "应累积 2 条 goal steering Defer（两次 active），终态 r2 不 push"
);
```

- [ ] **Step 3: 运行测试**

```bash
cargo test -p peri-middlewares --lib goal_middleware_test -- --nocapture
```
Expected: 4 个测试全部 PASS：
- `test_render_steering_escalates`
- `test_render_steering_contains_objective`
- `test_collect_tools_returns_goal_tool`
- `test_after_agent_goal_active_注入_steering_并设_block_continue`
- `test_after_agent_no_goal_放行_不注入`
- `test_after_agent_existing_block_continue_不干预`
- `test_after_agent_terminal_重置_pending_rounds`

- [ ] **Step 4: Commit**

```bash
git add peri-middlewares/src/goal_middleware_test.rs
git commit -m "test(goal): update tests for Defer-based steering injection

drain_for_receive 不消费 Defer，测试改用 drain_for_end 验证。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 3: 验证 End 阶段兼容性

**Files:**
- Read: `peri-agent/src/agent/stages/end.rs` (验证现有逻辑)
- Read: `peri-agent/src/session/queue.rs:163-208` (验证 drain 语义)

- [ ] **Step 1: 确认 End 阶段已有 Defer 支持**

End 阶段 `drain_for_end()` 已原生支持 `Defer`：`MessageKind::Prompt | MessageKind::Defer => consumed.push(msg)`。无需修改 End 代码。

验证命令（确认现有测试仍然通过）：
```bash
cargo test -p peri-agent --lib stages::end::tests -- --nocapture
```
Expected: 所有 End 阶段测试 PASS（`test_end_defer_wakes` 等）。

- [ ] **Step 2: 确认 run_react_loop 中 Defer→transcript 路径**

`stages/mod.rs:671-698` 已处理 `end_out.should_continue && !awakened_messages.is_empty()` → `append_messages_to_transcript`。Defer 消息被写入 transcript 后循环继续。无需修改。

验证：运行完整 peri-agent 测试确认无回归：
```bash
cargo test -p peri-agent --lib 2>&1 | tail -20
```
Expected: 全部通过，0 failures。

- [ ] **Step 3: Commit**

(此任务仅为验证，无代码改动，无需 commit)

---

### Task 4: 端到端验证

- [ ] **Step 1: Full workspace build**

```bash
cargo build --workspace 2>&1
```
Expected: 编译通过。

- [ ] **Step 2: Full workspace test**

```bash
cargo test --workspace --lib 2>&1 | tail -30
```
Expected: 全部通过，0 failures。

- [ ] **Step 3: 运行 goal 相关完整测试**

```bash
cargo test -p peri-agent goal --lib -- --nocapture
cargo test -p peri-acp goal --lib -- --nocapture
cargo test -p peri-middlewares goal --lib -- --nocapture
```
Expected: 全部通过。

---

## Self-Review

**1. Spec coverage:**
- ✅ 症状 1 "goal active 后一轮退出" → Task 1 修复（Defer 触发续跑）
- ✅ 症状 2 "block_continue 被丢弃" → 不再依赖 block_continue，走 Defer 路径
- ✅ 症状 3 "complete/block 后状态转换无法驱动续跑" → complete/block 后 is_goal_active=false，不 push Defer，循环自然退出

**2. Placeholder scan:** 无 TBD/TODO/实现细节占位符。

**3. Type consistency:**
- `MessageKind::Defer` 与 `drain_for_end` 的 `consumed.push(msg)` 类型匹配
- `drain_for_end()` 返回 `Option<Vec<QueuedMessage>>`，测试中 `.unwrap_or_default()` 正确
- 无新增 API/类型，无跨 task 签名不一致问题
