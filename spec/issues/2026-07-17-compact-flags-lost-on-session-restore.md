# Compact 标记（truncated/excluded）在 Session 恢复后丢失，导致上下文直接到 100%

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-17

## 问题描述

长对话中 Micro Compact 触发后，StatusBar 上下文使用率降到合理值（如 40%）。此时停止对话，再恢复 session 继续发 prompt，上下文使用率会瞬间从 40% 跳到 100%。原因是 compact 阶段设置的消息标记（`truncated`/`excluded`）虽然在会话期间正确持久化到了 SQLite，但在 session 恢复时未被加载回 transcript，导致 LLM 收到了完整消息内容而非截断后的版本。

## 症状详情

| 时刻 | StatusBar 上下文 | 说明 |
|------|-----------------|------|
| Micro Compact 前 | 70%-85% | 上下文使用率升高，触发 micro compact |
| Micro Compact 后，LLM 回复后 | ~40% | 截断后的消息减少 input_tokens，StatusBar 缓存此值 |
| 停止对话 + 重新 resume | ~40% | TUI 层缓存了上次 StatusBar 显示值（v2 中 TokenTracker 跨 session 不复用） |
| 发新 prompt 后 | 立即 100% | LLM 收到完整内容（标记丢失），`last_usage.input_tokens` 暴涨 |

关键症状：**resume 后的第一个 LLM 回复就显示 100%**，说明消息以完整内容被发送，而非截断版本。

## 根因分析

存在**两个独立根因**，都导致标记在恢复时丢失：

### 根因 A：DB 加载路径不读标记列

`SqliteThreadStore` 的三条加载路径都只 `SELECT content`，不读 `truncated`/`excluded`：

| 方法 | 位置 | 查询 |
|------|------|------|
| `load_messages()` | `sqlite_store.rs:352-362` | `SELECT content FROM messages ...` |
| `load_messages_up_to()` | `sqlite_store.rs:150-181` | `SELECT content FROM messages ...` |
| `load_context()` 增量加载 | `sqlite_store.rs:484-489` | `SELECT content FROM messages ... LIMIT -1 OFFSET ...` |

标记在会话期间正确写入 DB（`transcript.rs:359-377` → `PersistOp::UpdateFlags` → `update_message_flags` → `UPDATE messages SET truncated=?, excluded=?`），但恢复时从未读回。

### 根因 B：`cached_context` 序列化格式不支持标记

`save_context_cache()`（`sqlite_store.rs:184-198`）将 `Vec<BaseMessage>` 序列化为 JSON 存入 `cached_context`。`BaseMessage` 结构体本身**不含 `truncated`/`excluded` 字段**，标记只存在于 `MessageTranscript.flags: HashMap<MessageId, MessageFlags>` 中。因此 `cached_context` 格式在**结构上就无法保存标记**。

即使修复了根因 A（DB 查询加读标记列），`load_context()` 的缓存命中路径（`sqlite_store.rs:480-503`）仍会丢失标记。

### 子问题：`update_message_flags` 不失效 `cached_context`

标记写入 DB 后未调用 `invalidate_context_cache()`，导致缓存变脏——如果 `cached_context` 在标记写入前已填充，恢复时仍会加载到无标记的旧缓存。

### 完整丢失链路

```
Micro Compact (运行中)
  → transcript.set_truncated() 更新内存 flags HashMap ✓
  → PersistOp::UpdateFlags → update_message_flags() 写入 DB ✓
  → 会话结束，transcript 销毁，内存 flags 丢失

Session 恢复
  → load_session_messages() → load_context()
    ├─ cached_context 命中：JSON 反序列化 Vec<BaseMessage>（无 flags） ✗
    └─ 缓存未命中：SELECT content（无 flags） ✗
  → 返回 Vec<BaseMessage>，标记全部丢失
  → Phase 5: transcript.append_batch(history)
  → 消息进入 transcript，flags HashMap 初始为空 ✗
  → LLM 请求时 get_flags() 返回默认值（truncated=false, excluded=false）
  → 完整内容发送给 LLM → input_tokens 暴涨 → StatusBar 100%
```

### 影响范围

| 标记类型 | 受影响的 Compact | 严重程度 |
|----------|-----------------|----------|
| `truncated` | Micro Compact | **高** — 截断消息以完整内容发送，上下文直接跳到 100% |
| `excluded` | Full Compact | **高** — 已排除的消息重新出现，可能超过 context window 导致 API 报错 |

## 修复方案

### 推荐：方案 A（trait 增加 `load_message_flags()` 方法）

**文件 1：`peri-agent/src/thread/store.rs`** — trait 增加方法（带默认 no-op 实现）
```rust
async fn load_message_flags(
    &self,
    thread_id: &ThreadId,
) -> Result<HashMap<MessageId, MessageFlags>> {
    // 默认实现：无持久化后端时返回空
    Ok(HashMap::new())
}
```

**文件 2：`peri-agent/src/thread/sqlite_store.rs`** — 实现
```rust
async fn load_message_flags(&self, thread_id: &ThreadId) -> Result<HashMap<MessageId, MessageFlags>> {
    let rows: Vec<(String, bool, bool)> = sqlx::query_as(
        "SELECT message_id, truncated, excluded FROM messages WHERE thread_id = ?1 AND (truncated = 1 OR excluded = 1)"
    )
    .bind(thread_id.as_str())
    .fetch_all(&self.pool)
    .await?;
    // 构建 HashMap …
}
```

**文件 3：`peri-agent/src/session/transcript.rs`** — 增加 `set_flags_batch()` 方法
```rust
pub fn set_flags_batch(&mut self, flags: HashMap<MessageId, MessageFlags>) {
    for (id, flag) in flags {
        if flag != MessageFlags::default() {
            self.flags.insert(id, flag);
        }
    }
}
```

**文件 4：`peri-acp/src/session/executor_helpers.rs`** — Phase 5 之后恢复标记
Phase 6.7 之后、Phase 7 之前新增 Phase 5.5：
- 从 `thread_store.load_message_flags(thread_id)` 读取标记
- 写入 transcript：`transcript.set_flags_batch(flags)`

**文件 5：`peri-agent/src/session/transcript.rs:161-165`** — 子问题修复（一行改动）
在 persist writer task 中 `UpdateFlags` 分支之后增加缓存失效：
```rust
PersistOp::UpdateFlags(id, flags) => {
    store.update_message_flags(&id, flags.truncated, flags.excluded).await?;
    store.invalidate_context_cache(&tid).await?;  // ← 新增一行
}
```
writer task 已捕获 `tid`（`thread_id`），无需修改 trait 签名。

### 备选：方案 B（改 `load_context()` 返回类型）

改 `load_context()` 返回 `Vec<(BaseMessage, MessageFlags)>`，将标记与消息一起返回。**破坏性变更**，影响 8+ 文件和 trait 签名（`ThreadStore` trait、所有 impl、所有调用方、`cached_context` 序列化格式），不推荐。

### 方案 A 安全性分析

| 考虑点 | 结论 |
|--------|------|
| Phase 5 `append_batch` 重持久化 | 安全 — `INSERT OR IGNORE` + UUID message_id 保证幂等 |
| Phase 5 与 Phase 5.5 竞态 | 无竞态 — `INSERT OR IGNORE` 不变更已存在的行 |
| message_id 一致性 | 一致 — history 来自 `load_context()`，id 与 DB `message_id` 同源 |
| 新对话（无 thread_store） | `store` 为 `None` 时跳过，正确 |
| SubAgent 路径 | 不受影响 — SubAgent 走 `with_ancestor()` 路径，无独立 ThreadStore |

## 涉及文件

- `peri-agent/src/thread/sqlite_store.rs:352-362` —— `load_messages()` 只 SELECT content
- `peri-agent/src/thread/sqlite_store.rs:150-181` —— `load_messages_up_to()` 同上
- `peri-agent/src/thread/sqlite_store.rs:470-504` —— `load_context()` 缓存路径丢失标记（根因 B）
- `peri-agent/src/thread/sqlite_store.rs:184-198` —— `save_context_cache()` 格式不含标记（根因 B）
- `peri-agent/src/thread/sqlite_store.rs:649-663` —— `update_message_flags()` 不失效缓存（子问题）
- `peri-acp/src/session/executor_helpers.rs:671-676` —— Phase 5 `append_batch(history)` 不带标记
- `peri-agent/src/session/transcript.rs:359-377` —— `set_truncated()`/`set_excluded()` 写入路径正常
- `peri-agent/src/session/transcript.rs:130-141` —— `with_ancestor()` 不设置 flags（SubAgent 路径正常）
- `peri-agent/src/thread/store.rs` —— trait 需新增 `load_message_flags()`

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-17 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
