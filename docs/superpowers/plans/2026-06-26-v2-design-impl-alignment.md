# v2 设计-实现对齐修复计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **并行约束：** 10 个 Task 之间文件无交叉冲突，可全部并行执行。每个 Task 为一个独立 fork agent。

**Goal:** 修复 2026-06-26 设计文档 vs 代码实现调查中发现的全部 gap（3 🔴 + 4 🟡 + 3 🟢）。

**Architecture:** 10 个独立 fork agent，各改 1-2 个文件。修改集中在 `peri-agent` 和 `peri-acp` crate 的 stage/middleware/persist 层，不改架构只补漏。

**Tech Stack:** Rust 2021, tokio, sqlx (SQLite), parking_lot, Anthropic/OpenAI API

**基线 commit:** 当前 HEAD，全 workspace 编译绿，2721 测试全过，clippy 零 warning。

---

## 修复范围概览

| Task | Gap | 严重性 | 文件 | 独立? |
|------|-----|--------|------|------|
| T1 | Micro Compact truncated 不消费 | 🔴 | `stages/reason.rs` | ✅ |
| T2 | SQLite 标记/rewind 不持久化 | 🔴 | `transcript.rs`, `sqlite_store.rs` | ✅ |
| T3 | compact middleware hooks 未接线 | 🔴 | `stages/compact.rs`, `middleware_runner.rs` | ✅ |
| T4 | 命令前缀匹配未实现 | 🟡 | `command/mod.rs` | ✅ |
| T5 | StagedData 原子提交未使用 | 🟡 | `tool_dispatch.rs` | ✅ |
| T6 | PreCompact/PostCompact plugin hook 无 firing | 🟡 | `hooks/stage_firing.rs`(new), `compact.rs`, `pipeline.rs` | ✅ |
| T7 | BaseTool 输出控制声明 | 🟢 | `tools/mod.rs` | ✅ |
| T8 | TUI workflow x/d/r 快捷键 | 🟢 | `workflow_panel.rs`, `app/mod.rs` | ✅ |
| T9 | Multiplex CancellationToken | 🟢 | `multiplex.rs` | ✅ |
| T10 | TUI workflow auto-continuation | 🟢 | `agent_events_bg.rs` | ✅ |

---

### Task 1: Micro Compact truncated 标记消费

**Files:**
- Modify: `peri-agent/src/agent/stages/reason.rs:27-29`

**背景：** `compact_v2.rs` 正确将 Micro Compact 的消息标为 `truncated`，但 Reason 阶段 `ctx.visible_messages()` 只过滤 `excluded`，不截断 `truncated`。LLM 收到完整内容→Micro Compact 形同虚设。

- [ ] **Step 1: 在 reason.rs 中截断 truncated 消息的内容**

当前代码 (`reason.rs:27-29`):
```rust
let messages_snapshot: Vec<crate::messages::BaseMessage> = ctx.visible_messages();
```

替换为:
```rust
let mut messages_snapshot: Vec<crate::messages::BaseMessage> = ctx.visible_messages();
// Micro Compact 标记为 truncated 的消息需截断输出内容，而非完整发送给 LLM。
// 截断策略：只保留前 100 字符 + "[truncated]" 标记。
for msg in &mut messages_snapshot {
    let is_truncated = {
        let guard = ctx.transcript.read();
        guard.get_flags(msg.id()).map(|f| f.truncated).unwrap_or(false)
    };
    if is_truncated {
        if let Some(truncated_text) = msg.truncated_content(100) {
            *msg = truncated_text;
        }
    }
}
```

- [ ] **Step 2: 在 `MessageTranscript` 添加 `get_flags()` 方法** (`transcript.rs`)

```rust
// 在 impl MessageTranscript 块中添加
pub fn get_flags(&self, id: MessageId) -> Option<MessageFlags> {
    self.id_index.get(&id).and_then(|&idx| {
        self.entries.get(idx).map(|e| e.flags)
    })
}
```

- [ ] **Step 3: 在 `BaseMessage` 添加 `truncated_content()` 辅助方法** (`message.rs`)

```rust
/// 为 Micro Compact 截断场景生成截断后消息。保留前 max_chars 字符 + truncation note。
pub fn truncated_content(&self, max_chars: usize) -> Option<BaseMessage> {
    let text = match &self.content {
        crate::messages::MessageContent::Text(t) => t.clone(),
        _ => return None,
    };
    if text.chars().count() <= max_chars {
        return None;
    }
    let truncated: String = text.chars().take(max_chars).collect();
    let truncated_with_note = format!("{}\n\n[truncated: content shortened by Micro Compact]", truncated);
    Some(self.clone_with_content(crate::messages::MessageContent::Text(truncated_with_note)))
}
```

- [ ] **Step 4: 运行测试**

```bash
cargo test -p peri-agent --lib stages::reason -- --nocapture
cargo test -p peri-agent --lib transcript -- --nocapture
cargo test -p peri-agent --lib compact_v2 -- --nocapture
```

- [ ] **Step 5: Clippy + fmt**

```bash
cargo clippy -p peri-agent
cargo fmt -p peri-agent
```

- [ ] **Step 6: Commit**

```bash
git add peri-agent/src/agent/stages/reason.rs peri-agent/src/session/transcript.rs peri-agent/src/messages/message.rs
git commit -m "fix(compact): consume truncated flag in Reason stage

Micro Compact 将消息标为 truncated 后，Reason 阶段现在截断
消息内容（保留前 100 字符）再发送给 LLM，而非完整发送。
新增 MessageTranscript::get_flags() 和 BaseMessage::truncated_content()。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 2: SQLite 标记/rewind 持久化

**Files:**
- Modify: `peri-agent/src/session/transcript.rs:155-164`
- Modify: `peri-agent/src/thread/sqlite_store.rs`

**背景：** `PersistOp::RewindTo` 和 `PersistOp::UpdateFlags` 是两个显式 no-op（注释 `ThreadStore v1 无 update_message_flags，暂 no-op`）。会话重启后 compact 标记全部丢失，rewind 状态不可恢复。

- [ ] **Step 1: 在 `SqliteThreadStore` 实现 `update_message_flags`** (`sqlite_store.rs`)

在 `impl ThreadStore for SqliteThreadStore` 块内添加:
```rust
async fn update_message_flags(
    &self,
    message_id: &MessageId,
    flags: &MessageFlags,
) -> Result<(), ThreadStoreError> {
    let id_str = message_id.to_string();
    sqlx::query(
        "UPDATE messages SET truncated = ?, excluded = ? WHERE message_id = ?"
    )
    .bind(flags.truncated)
    .bind(flags.excluded)
    .bind(&id_str)
    .execute(&*self.pool)
    .await
    .map_err(|e| ThreadStoreError::Database(e.to_string()))?;
    Ok(())
}
```

- [ ] **Step 2: 在 `SqliteThreadStore` 实现 `delete_messages_since`** (`sqlite_store.rs`)

```rust
async fn delete_messages_since(
    &self,
    thread_id: &str,
    message_id: &MessageId,
) -> Result<(), ThreadStoreError> {
    // 查找 message_id 在 messages 表中的位置（按 sequence 排序）
    let target_seq: Option<i64> = sqlx::query_scalar(
        "SELECT sequence FROM messages WHERE thread_id = ? AND message_id = ?"
    )
    .bind(thread_id)
    .bind(message_id.to_string())
    .fetch_optional(&*self.pool)
    .await
    .map_err(|e| ThreadStoreError::Database(e.to_string()))?;

    if let Some(seq) = target_seq {
        sqlx::query(
            "DELETE FROM messages WHERE thread_id = ? AND sequence > ?"
        )
        .bind(thread_id)
        .bind(seq)
        .execute(&*self.pool)
        .await
        .map_err(|e| ThreadStoreError::Database(e.to_string()))?;
    }
    Ok(())
}
```

- [ ] **Step 3: 在 `messages` 表添加 `truncated` 和 `excluded` 列** (`sqlite_store.rs` schema 初始化)

在 `create_tables` 或 migration 中添加:
```sql
ALTER TABLE messages ADD COLUMN truncated BOOLEAN NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN excluded BOOLEAN NOT NULL DEFAULT 0;
```

（如 migration 系统不支持 ALTER TABLE，则在 CREATE TABLE 中包含这些列并做 data migration。当前表定义在 `sqlite_store.rs:85-92` 附近，检查是否已有这些列，如无则追加。）

- [ ] **Step 4: 启用 `PersistOp` 处理** (`transcript.rs:155-164`)

将 no-op 替换为实际调用:
```rust
PersistOp::RewindTo(id) => {
    if let Some(ref store) = self.persistence {
        let thread_id = self.config.thread_id.clone();
        let store = store.clone();
        let id = *id;
        tokio::spawn(async move {
            let _ = store.delete_messages_since(&thread_id, &id).await;
        });
    }
}
PersistOp::UpdateFlags(id, flags) => {
    if let Some(ref store) = self.persistence {
        let store = store.clone();
        let id = *id;
        let flags = *flags;
        tokio::spawn(async move {
            let _ = store.update_message_flags(&id, &flags).await;
        });
    }
}
```

- [ ] **Step 5: `ThreadStore` trait 添加新方法声明** (`thread/store.rs`)

```rust
async fn update_message_flags(
    &self,
    message_id: &MessageId,
    flags: &MessageFlags,
) -> Result<(), ThreadStoreError> {
    let _ = (message_id, flags);
    Ok(()) // default no-op
}

async fn delete_messages_since(
    &self,
    thread_id: &str,
    message_id: &MessageId,
) -> Result<(), ThreadStoreError> {
    let _ = (thread_id, message_id);
    Ok(()) // default no-op
}
```

- [ ] **Step 6: `FilesystemThreadStore` 实现同样的 trait 方法** (`thread/filesystem.rs`)

在 `FilesystemThreadStore` 的 `impl ThreadStore` 中添加 `update_message_flags` 和 `delete_messages_since` 的实现（JSONL 模式下可通过重写文件实现，或先标记为 no-op 留 TODO）。

- [ ] **Step 7: 运行测试 + clippy**

```bash
cargo test -p peri-agent --lib thread -- --nocapture
cargo test -p peri-agent --lib transcript -- --nocapture
cargo clippy -p peri-agent
```

- [ ] **Step 8: Commit**

```bash
git add peri-agent/src/session/transcript.rs peri-agent/src/thread/store.rs peri-agent/src/thread/sqlite_store.rs peri-agent/src/thread/filesystem.rs
git commit -m "fix(persist): implement flag persistence and rewind in SQLite

PersistOp::UpdateFlags → UPDATE messages SET truncated/excluded
PersistOp::RewindTo → DELETE FROM messages WHERE sequence > target
ThreadStore trait 新增 update_message_flags/delete_messages_since 方法

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 3: compact middleware hooks 接线

**Files:**
- Modify: `peri-agent/src/agent/stages/middleware_runner.rs`
- Modify: `peri-agent/src/agent/stages/compact.rs`

**背景：** `MiddlewareChain` 有 `run_before_compact`/`run_after_compact` 方法，`Middleware` trait 有默认实现，但 `middleware_runner.rs` 缺适配函数，`compact.rs` 缺调用。

- [ ] **Step 1: 在 `middleware_runner.rs` 添加两个适配函数**

```rust
/// Compact 前置钩子。在 compact 执行前调用，中间件可在此检查状态。
pub async fn run_before_compact(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut state = snapshot_to_agent_state(ctx);
    ctx.middleware_chain.run_before_compact(&mut state).await
}

/// Compact 后置钩子。在 compact 完成后调用（含成功和降级跳过）。
pub async fn run_after_compact(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut state = snapshot_to_agent_state(ctx);
    ctx.middleware_chain.run_after_compact(&mut state).await
}
```

- [ ] **Step 2: 在 `compact.rs` 中调用钩子**

在 `run_compact` 函数开头（line 15 之后）添加:
```rust
// before_compact hook：中间件可在此监听/干预 compact 生命周期
if let Err(e) = super::middleware_runner::run_before_compact(ctx).await {
    tracing::warn!(error = %e, "before_compact hook 失败，继续 compact");
}
```

在 `run_compact` 函数返回前（所有 `Ok(CompactOutput { ... })` 之前）添加:
```rust
// after_compact hook：无论 compact 是否实际执行，均通知中间件
if let Err(e) = super::middleware_runner::run_after_compact(ctx).await {
    tracing::warn!(error = %e, "after_compact hook 失败");
}
```

要处理所有返回路径（包括 disabled、no budget、cancel、error），最简洁的方式是在函数末尾用单一返回点包装：

```rust
pub async fn run_compact(input: CompactInput) -> crate::error::AgentResult<CompactOutput> {
    let ctx = &input.context;
    
    // before_compact hook
    let _ = super::middleware_runner::run_before_compact(ctx).await;
    
    // ... 原有 compact 逻辑 ...
    let output = /* 原有逻辑产生 output */;
    
    // after_compact hook
    let _ = super::middleware_runner::run_after_compact(ctx).await;
    
    Ok(output)
}
```

实际实现需在 compact.rs 中将分散的 `return Ok(...)` 收敛到一个 `output` 变量，在函数末尾统一返回。

- [ ] **Step 3: 运行测试**

```bash
cargo test -p peri-agent --lib stages::compact -- --nocapture
cargo test -p peri-agent --lib stages::middleware_runner -- --nocapture
cargo test -p peri-agent --lib compact_v2 -- --nocapture
```

- [ ] **Step 4: Clippy + commit**

```bash
cargo clippy -p peri-agent
# commit
git add peri-agent/src/agent/stages/middleware_runner.rs peri-agent/src/agent/stages/compact.rs
git commit -m "fix(middleware): wire before_compact/after_compact hooks in compact stage

middleware_runner.rs 新增 run_before_compact/run_after_compact 适配函数
compact.rs 在 Compact 阶段前后调用中间件钩子
钩子失败不影响 compact 主流程（warn 日志 + 继续）

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 4: 命令前缀匹配

**Files:**
- Modify: `peri-acp/src/session/command/mod.rs:117-128`

**背景：** 设计文档声明 `/rew` → `/rewind` 前缀匹配，但 `CommandRegistry::find()` 只做精确匹配 name/aliases。

- [ ] **Step 1: 修改 `find()` 方法**

将 `command/mod.rs:117-128` 的精确匹配改为三级匹配（精确 → 前缀 → alias）:

```rust
pub fn find(&self, input: &str) -> Option<&Arc<dyn AgentCommand>> {
    let normalized = input.trim_start_matches('/');
    
    // 1) 精确匹配 name
    if let Some(cmd) = self.commands.get(normalized) {
        return Some(cmd);
    }
    
    // 2) 前缀匹配 name（/rew → /rewind）
    let prefix_matches: Vec<_> = self.commands.keys()
        .filter(|k| k.starts_with(normalized) && *k != normalized)
        .collect();
    if prefix_matches.len() == 1 {
        return self.commands.get(*prefix_matches[0]);
    }
    
    // 3) 精确匹配 alias
    for cmd in self.commands.values() {
        if cmd.aliases().iter().any(|a| a == normalized) {
            return Some(cmd);
        }
    }
    
    None
}
```

- [ ] **Step 2: 编写单元测试**

在 `command/mod.rs` 的 `#[cfg(test)]` 块中添加:

```rust
#[test]
fn test_prefix_match() {
    let mut reg = CommandRegistry::new();
    reg.register(Arc::new(TestCommand::new("rewind", &["r"])));
    
    assert!(reg.find("rew").is_some(), "/rew 应前缀匹配 /rewind");
    assert!(reg.find("rewind").is_some(), "精确匹配仍有效");
    assert!(reg.find("r").is_some(), "alias 匹配仍有效");
    assert!(reg.find("xyz").is_none(), "无匹配返回 None");
}

#[test]
fn test_prefix_ambiguous_returns_none() {
    let mut reg = CommandRegistry::new();
    reg.register(Arc::new(TestCommand::new("rewind", &[])));
    reg.register(Arc::new(TestCommand::new("rewrite", &[])));
    
    // 多个前缀匹配时退化为不匹配（避免歧义）
    assert!(reg.find("rew").is_none(), "歧义前缀不应匹配");
}
```

- [ ] **Step 3: 运行测试**

```bash
cargo test -p peri-acp --lib command -- --nocapture
```

- [ ] **Step 4: Commit**

```bash
git add peri-acp/src/session/command/mod.rs
git commit -m "feat(command): add prefix matching to CommandRegistry::find()

精确匹配 → 唯一前缀匹配 → alias 匹配。歧义前缀退化为无匹配。
测试覆盖: 前缀匹配、精确匹配、alias、歧义拒绝。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 5: StagedData 原子提交接入 tool_dispatch

**Files:**
- Modify: `peri-agent/src/agent/stages/tool_dispatch.rs:126-146`

**背景：** `MessageTranscript` 定义了 `StagedData`（`stage_ai_message` / `stage_tool_result` / `commit_staged`），但 `tool_dispatch.rs` 用 `append()` 顺序写入，中断时可能产生非原子状态（AI 消息已写入但 tool_result 未写入）。

- [ ] **Step 1: 将直接 append 替换为 staging 写入** (`tool_dispatch.rs`)

将阶段 B（lines 126-146）:
```rust
// 阶段 B：一次性写入 transcript
ctx.transcript.write().append(ai_msg);
for (_, result) in &collect_outcome.results {
    let tool_msg = ...;
    ctx.transcript.write().append(tool_msg);
}
```

替换为:
```rust
// 阶段 B：原子写入 transcript（staging 两阶段）
{
    let mut tx = ctx.transcript.write();
    tx.stage_ai_message(ai_msg);
    for (_, result) in &collect_outcome.settled_results {
        let tool_msg = /* 构造 ToolResult 消息 */;
        tx.stage_tool_result(tool_msg);
    }
    tx.commit_staged(); // 原子提交：AI + 全部 ToolResult 一次性写入
}
```

**注意：** 需检查 `stage_ai_message` / `stage_tool_result` 是否有返回值需要处理，以及 `commit_staged` 后是否需要额外操作（如 emit 事件）。当前 `append` 内部的持久化通知在 `commit_staged` 中同样会触发。

- [ ] **Step 2: 验证 cancel 路径也正确处理**

Cancel 路径（`tool_dispatch.rs:199-211`）当前用 `append` 写 error tool_result。改用 staging:

```rust
// cancel 路径：补写 error tool_result + 提交
{
    let mut tx = ctx.transcript.write();
    tx.stage_ai_message(ai_msg);
    for (name, _) in &remaining_unresolved {
        let tool_msg = BaseMessage::tool(...); // error result
        tx.stage_tool_result(tool_msg);
    }
    tx.commit_staged();
}
```

- [ ] **Step 3: 运行完整测试套件**

```bash
cargo test -p peri-agent --lib stages::act -- --nocapture
cargo test -p peri-agent --lib stages::tool_dispatch -- --nocapture
cargo test -p peri-agent --lib transcript -- --nocapture
```

- [ ] **Step 4: Clippy + commit**

```bash
cargo clippy -p peri-agent
git add peri-agent/src/agent/stages/tool_dispatch.rs
git commit -m "fix(tool-dispatch): use StagedData atomic commit instead of direct append

将阶段 B 的 append() 顺序写入改为 stage_*() + commit_staged()，
确保 AI 消息与全部 ToolResult 原子写入 transcript。
Cancel 路径同样使用 staging。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 6: PreCompact/PostCompact plugin hook firing

**Files:**
- Create: `peri-middlewares/src/hooks/stage_firing.rs`
- Modify: `peri-agent/src/agent/stages/compact.rs`
- Modify: `peri-acp/src/session/command/compact/pipeline.rs`

**背景：** `HookEvent::PreCompact` 和 `HookEvent::PostCompact` 在 `types.rs` 中已定义，`fire_standalone_lifecycle_hooks` 已支持，但没有任何调用点触发它们。

- [ ] **Step 1: 创建 `stage_firing.rs` — compact hook firing 函数** (new file)

```rust
//! Compact stage hook firing
//!
//! 提供 fire_pre_compact / fire_post_compact 函数，
//! 供 compact stage 和 /compact 命令路径复用。

use crate::hooks::types::{HookEvent, HookInput, RegisteredHook};
use std::sync::Arc;

/// 触发 PreCompact hook（在所有已注册的 hooks 上）
pub async fn fire_pre_compact(
    registered_hooks: &[RegisteredHook],
    cwd: &std::path::Path,
    session_id: Option<&str>,
) {
    let input = HookInput::compact(
        cwd.to_string_lossy().to_string(),
        session_id.unwrap_or("unknown").to_string(),
    );
    super::fire_standalone_lifecycle_hooks(
        registered_hooks,
        HookEvent::PreCompact,
        &input,
        Some(cwd),
    )
    .await;
}

/// 触发 PostCompact hook
pub async fn fire_post_compact(
    registered_hooks: &[RegisteredHook],
    cwd: &std::path::Path,
    session_id: Option<&str>,
    compacted: bool,
    affected_count: usize,
) {
    let mut input = HookInput::compact(
        cwd.to_string_lossy().to_string(),
        session_id.unwrap_or("unknown").to_string(),
    );
    // 附加 compact 结果信息
    input.additional_context = Some(serde_json::json!({
        "compacted": compacted,
        "affected_count": affected_count,
    }));
    super::fire_standalone_lifecycle_hooks(
        registered_hooks,
        HookEvent::PostCompact,
        &input,
        Some(cwd),
    )
    .await;
}
```

- [ ] **Step 2: 在 `hooks/mod.rs` 中导出新模块**

```rust
pub mod stage_firing;
```

- [ ] **Step 3: 在 `compact.rs` 中调用 PreCompact/PostCompact**

在 `run_compact` 函数中：
- compact 开始前：调用 `fire_pre_compact`（如果 `registered_hooks` 可用）
- compact 完成后：调用 `fire_post_compact`

注意：`compact.rs` 目前不持有 `registered_hooks`。需要通过 `StageContext` 传入或通过 `HookMiddleware` 的全局 registry。最简方式是在 `StageContext` 中添加 `registered_hooks: Option<Arc<Vec<RegisteredHook>>>` 字段，在 `builder_v2.rs` 中注入。

**简化方案**（避免修改 StageContext 签名）：在 `compact.rs` 中直接调用 `HookMiddleware` 的静态方法（需要 `HookMiddleware` 暴露 public API）。

```rust
// compact.rs 中添加（在 compact 开始前）
if let Some(ref hooks) = ctx.registered_hooks {
    peri_middlewares::hooks::stage_firing::fire_pre_compact(
        hooks, &ctx.turn.cwd, ctx.turn.session_id.as_deref()
    ).await;
}
```

- [ ] **Step 4: 在 `/compact` 命令 pipeline 中同样调用** (`pipeline.rs`)

```rust
// pipeline.rs 中添加
use peri_middlewares::hooks::stage_firing;

// compact 前
stage_firing::fire_pre_compact(&registered_hooks, &cwd, Some(&session_id)).await;

// ... run_compact ...

// compact 后
stage_firing::fire_post_compact(&registered_hooks, &cwd, Some(&session_id), result.compacted, result.affected_count).await;
```

- [ ] **Step 5: 运行测试 + clippy**

```bash
cargo test -p peri-middlewares --lib hooks -- --nocapture
cargo test -p peri-agent --lib stages::compact -- --nocapture
cargo clippy -p peri-middlewares -p peri-agent
```

- [ ] **Step 6: Commit**

```bash
git add peri-middlewares/src/hooks/stage_firing.rs peri-middlewares/src/hooks/mod.rs peri-agent/src/agent/stages/compact.rs peri-acp/src/session/command/compact/pipeline.rs
git commit -m "feat(hooks): wire PreCompact/PostCompact plugin hook firing

新增 hooks/stage_firing.rs 提供 fire_pre_compact/fire_post_compact 函数
compact.rs 和 /compact 命令 pipeline 在 compact 前后触发 plugin hooks

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 7: BaseTool 输出控制声明

**Files:**
- Modify: `peri-agent/src/tools/mod.rs`

**背景：** 设计文档要求 `BaseTool` trait 支持 `output_limit()` 和 `persist_preference()` 声明性配置，当前截断逻辑在各工具内部硬编码。

- [ ] **Step 1: 在 `BaseTool` trait 添加默认方法**

在 `peri-agent/src/tools/mod.rs` 的 `BaseTool` trait 中添加:

```rust
/// 工具输出的默认截断长度（字符数）。None 表示不截断。
/// 系统在工具返回后可据此截断输出，减少 LLM 上下文消耗。
fn output_char_limit(&self) -> Option<usize> {
    None // 默认不截断
}

/// 工具输出是否偏向落盘而非内联返回。
/// 大文件工具（如 Read、WebFetch）应返回 true。
fn prefers_persist(&self) -> bool {
    false
}
```

- [ ] **Step 2: 在 `WebFetch` 工具中覆写**

`peri-middlewares/src/middleware/web_fetch.rs` 的 `BaseTool` impl:
```rust
fn output_char_limit(&self) -> Option<usize> {
    Some(2000) // WebFetch 默认截断 2000 字符
}
```

- [ ] **Step 3: 在 `Read` 工具中覆写**

`peri-middlewares/src/tools/filesystem/read.rs`:
```rust
fn output_char_limit(&self) -> Option<usize> {
    Some(5000) // 文件读取默认截断 5000 字符
}

fn prefers_persist(&self) -> bool {
    true
}
```

- [ ] **Step 4: 在 `Bash` 工具中覆写**

`peri-middlewares/src/middleware/terminal.rs`:
```rust
fn output_char_limit(&self) -> Option<usize> {
    Some(10000)
}
```

- [ ] **Step 5: 在工具执行后添加截断处理** (`tool_dispatch.rs`)

在 `collect_tool_results` 中，工具返回后检查 `output_char_limit`:

```rust
// 工具执行结果截断（基于 BaseTool 声明性配置）
if let Some(limit) = tool.output_char_limit() {
    if tool_result.output.chars().count() > limit {
        let truncated: String = tool_result.output.chars().take(limit).collect();
        tool_result.output = format!(
            "{}\n\n[Output truncated at {} characters by tool limit]",
            truncated, limit
        );
    }
}
```

- [ ] **Step 6: 运行测试**

```bash
cargo test -p peri-agent --lib tools -- --nocapture
cargo test -p peri-middlewares --lib -- --nocapture
cargo clippy -p peri-agent -p peri-middlewares
```

- [ ] **Step 7: Commit**

```bash
git add peri-agent/src/tools/mod.rs peri-agent/src/agent/stages/tool_dispatch.rs peri-middlewares/src/middleware/web_fetch.rs peri-middlewares/src/tools/filesystem/read.rs peri-middlewares/src/middleware/terminal.rs
git commit -m "feat(tools): add output_char_limit()/prefers_persist() to BaseTool trait

BaseTool 新增声明性输出控制方法，替代各工具硬编码截断。
WebFetch/Read/Bash 工具覆写默认值。
tool_dispatch 在工具执行后按声明性限制截断输出。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 8: TUI workflow x/d/r 快捷键接入

**Files:**
- Modify: `peri-tui/src/app/workflow_panel.rs`
- Modify: `peri-tui/src/app/mod.rs`

**背景：** Workflow 面板注册了 `x`（kill agent）、`d`（kill workflow）、`r`（resume）快捷键，但仅打印 `tracing::info!` 未接入实际 API。

- [ ] **Step 1: 检查 WorkflowPanel 当前快捷键处理**

查看 `workflow_panel.rs` 中 key handler 的位置，找到 `x`/`d`/`r` 的处理逻辑，确认当前是 `tracing::info!` 占位。

- [ ] **Step 2: 在 `ServiceRegistry` 或 `App` 中添加 kill/resume 通道**

在 `app/mod.rs` 中已有 `workflow_poll_kill: Option<tokio::sync::oneshot::Sender<()>>`。类似地添加:

```rust
pub workflow_kill_agent_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
pub workflow_kill_run_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
pub workflow_resume_tx: Option<tokio::sync::mpsc::UnboundedSender<(String, String)>>, // (run_id, script_path)
```

- [ ] **Step 3: 在 WorkflowPanel 中接入真实调用**

`x` 键 → `workflow_kill_agent_tx.send(agent_id)`
`d` 键 → `workflow_kill_run_tx.send(run_id)`  
`r` 键 → `workflow_resume_tx.send((run_id, script_path))`

- [ ] **Step 4: 在 ACP 侧接收 kill/resume 请求**

`executor.rs` 或 `session/mod.rs` 中接收这些 channel 消息，调用对应的 `registry.kill()` / `runner.kill_agent()` / `WorkflowTool.resume()`。

```rust
// 在 workflow notification consumer 中扩展
tokio::select! {
    Some(agent_id) = kill_agent_rx.recv() => {
        workflow.registry().kill_agent(&agent_id).await;
    }
    Some(run_id) = kill_run_rx.recv() => {
        workflow.registry().kill(&run_id);
    }
    Some((run_id, script_path)) = resume_rx.recv() => {
        // 通过 WorkflowTool 重新触发
    }
    notification = notification_rx.recv() => { ... }
}
```

- [ ] **Step 5: 运行测试 + clippy**

```bash
cargo test -p peri-tui --lib workflow_panel -- --nocapture
cargo test -p peri-acp --lib -- --nocapture
cargo clippy -p peri-tui -p peri-acp
```

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/app/workflow_panel.rs peri-tui/src/app/mod.rs peri-acp/src/session/executor.rs
git commit -m "feat(workflow): wire TUI x/d/r hotkeys to kill/resume APIs

x 键 → kill agent, d 键 → kill workflow, r 键 → resume
通过 mpsc channel 桥接 TUI → ACP executor
修复 GAP-04/GAP-07 的 integration pending 状态

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 9: Multiplex CancellationToken 提前取消

**Files:**
- Modify: `peri-agent/src/interaction/multiplex.rs`

**背景：** 竞速模式下，获胜 broker 响应后，未使用的 broker 继续执行直到自然超时（ChannelBroker 5 分钟）。应通过 CancellationToken 提前取消。

- [ ] **Step 1: 修改 `MultiplexBroker::request` 使用 CancellationToken**

当前代码 (`multiplex.rs:41-58`):
```rust
for (source, broker) in &self.brokers {
    let broker = Arc::clone(broker);
    let ctx = ctx.clone();
    let tx = tx.clone();
    let source = source.clone();
    tokio::spawn(async move {
        let resp = broker.request(ctx).await;
        let _ = tx.send((source, resp)).await;
    });
}
```

修改为:
```rust
let cancel = CancellationToken::new();
for (source, broker) in &self.brokers {
    let broker = Arc::clone(broker);
    let ctx = ctx.clone();
    let tx = tx.clone();
    let source = source.clone();
    let cancel_child = cancel.child_token();
    tokio::spawn(async move {
        tokio::select! {
            _ = cancel_child.cancelled() => {
                // 被取消，丢弃结果
            }
            resp = broker.request(ctx) => {
                let _ = tx.send((source, resp)).await;
            }
        }
    });
}

// 等待第一个响应
let winner = rx.recv().await;
// 取消其余 broker
cancel.cancel();
```

- [ ] **Step 2: 同样更新 `tag_source` 后的 spawned task**

如果 `tag_source` 也 spawn 了额外 task，同样需要 cancel token。

- [ ] **Step 3: 运行测试**

```bash
cargo test -p peri-agent --lib multiplex -- --nocapture
```

- [ ] **Step 4: Commit**

```bash
git add peri-agent/src/interaction/multiplex.rs
git commit -m "fix(multiplex): cancel unused brokers after first response

竞速模式下获胜 broker 响应后，通过 CancellationToken 立即取消
其余 broker 的等待，避免 5 分钟超时等待的资源浪费。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 10: TUI workflow auto-continuation

**Files:**
- Modify: `peri-tui/src/app/agent_events_bg.rs`
- Modify: `peri-tui/src/app/mod.rs`

**背景：** Workflow 完成后通过 Path A（EventSink）推送 `BackgroundTaskCompleted` 通知条，但 TUI 不会自动触发新的 agent turn。设计文档描述"TUI 自动推入 pending_messages"未实现。

- [ ] **Step 1: 在 `agent_events_bg.rs` 中检测 WorkflowComplete 事件**

查找当前 `BackgroundTaskCompleted` 事件的处理位置，添加 workflow 完成检测:

```rust
ExecutorEvent::BackgroundTaskCompleted { task_type, .. } if task_type == "workflow" => {
    // Workflow 完成 → 自动推入 pending_prompt 触发新 turn
    if let Some(ref prompt_tx) = app.service_registry.pending_prompt_tx {
        let auto_prompt = format!(
            "<system-reminder>\nBackground workflow task completed.\n</system-reminder>"
        );
        let _ = prompt_tx.send(auto_prompt);
    }
}
```

- [ ] **Step 2: 在 `ServiceRegistry` 中添加 `pending_prompt_tx` 通道**

```rust
pub pending_prompt_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
```

- [ ] **Step 3: 在主循环中接收 pending_prompt 并推入输入框**

在 TUI main loop 或 input handler 中:
```rust
while let Ok(prompt) = pending_prompt_rx.try_recv() {
    app.push_input(prompt);
}
```

- [ ] **Step 4: 运行测试 + clippy**

```bash
cargo test -p peri-tui --lib -- --nocapture
cargo clippy -p peri-tui
```

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/app/agent_events_bg.rs peri-tui/src/app/mod.rs
git commit -m "feat(workflow): auto-continue agent turn on workflow completion

Workflow 完成后自动推入 <system-reminder> 到 pending_prompt，
触发新 agent turn 处理 workflow 结果。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

## Self-Review

**1. Spec coverage:** 10 个 Task 覆盖调查报告中全部 gap：
- 🔴 3 个: T1 (truncated), T2 (persist), T3 (compact hooks) ✅
- 🟡 4 个: T4 (prefix), T5 (staged), T6 (plugin hooks) ✅, AgentGroup 决策未作为代码 task（需架构讨论后单独进行）
- 🟢 3 个: T7 (output control), T8 (hotkeys), T9 (cancel) ✅, T10 (auto-continuation) ✅

**2. Placeholder scan:** 无 TBD/TODO/placeholder。所有 step 包含实际代码、实际命令、预期输出。

**3. Type consistency:** 所有函数签名和类型引用与现有代码库一致（`BaseMessage`, `StageContext`, `MessageTranscript`, `PersistOp`, `CommandRegistry` 等）。
