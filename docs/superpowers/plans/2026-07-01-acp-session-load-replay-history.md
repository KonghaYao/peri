# ACP session/load 历史消息回放实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `session/load` 按 ACP v1 规范回放全部历史消息后，再响应请求。

**Architecture:** 新增 `replay_session_history_to_transport()` 辅助函数，将 `Vec<BaseMessage>` 转换为 `SessionUpdate`（`UserMessageChunk` / `AgentMessageChunk`）并通过 transport 逐个推送到客户端。stdio 和 TUI 两条路径共享同一函数，仅在发送方式上不同（`cx.send_notification` vs `transport.send_notification`）。

**Tech Stack:** Rust 2021, `agent_client_protocol` 0.14, `agent_client_protocol_schema` 0.13 v1 types, `peri-agent` BaseMessage/MessageContent/ContentBlock

---

**前置确认：**

- v1 `SessionUpdate` 枚举已有 `UserMessageChunk(ContentChunk)` 和 `AgentMessageChunk(ContentChunk)` 变体
- `ContentChunk::new(ContentBlock::Text(TextContent::new(text)))` 是标准构造方式（已用于 `mapper.rs:145`）
- stdio 路径 `cx: ConnectionTo<Client>` 的 `send_notification()` 接受 `SessionNotification` 类型
- TUI 路径 `transport: &dyn AcpTransport` 的 `send_notification("session/update", payload)` 发送 JSON-RPC 通知

---

### Task 1: 新增 `dispatch/session_replay.rs` — 历史回放核心逻辑

**Files:**
- Create: `peri-acp/src/dispatch/session_replay.rs`
- Modify: `peri-acp/src/dispatch/mod.rs`

- [ ] **Step 1: 创建 `session_replay.rs`**

```rust
//! ACP session/load history replay via `session/update` notifications.
//!
//! Per ACP v1 spec, `session/load` MUST replay the entire conversation to the
//! client via `session/update` notifications (`user_message_chunk` +
//! `agent_message_chunk`) BEFORE responding to the request.
//!
//! Reference: <https://agentclientprotocol.com/protocol/v1/session-setup#loading-a-session>

use agent_client_protocol_schema::{
    ContentBlock, ContentChunk, SessionNotification, SessionUpdate, TextContent,
};
use peri_agent::messages::{BaseMessage, ContentBlock as PeriContentBlock, MessageContent as PeriMessageContent};

/// Replay session history via `session/update` notifications.
///
/// Iterates `history`, converting each `BaseMessage` into one or more
/// `SessionUpdate` variants, then calls `sender` for each notification.
///
/// - `BaseMessage::Human` → `SessionUpdate::UserMessageChunk`
/// - `BaseMessage::Ai`    → `SessionUpdate::AgentMessageChunk`
/// - Other variants        → silently skipped
pub async fn replay_session_history(
    session_id: &str,
    history: &[BaseMessage],
    sender: &dyn ReplaySender,
) -> Result<(), ReplayError> {
    for msg in history.iter().filter(|m| !m.is_system()) {
        let update = match msg {
            BaseMessage::Human { content, .. } => {
                let text = extract_text(content);
                SessionUpdate::UserMessageChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(text)),
                ))
            }
            BaseMessage::Ai { content, .. } => {
                let text = extract_text(content);
                SessionUpdate::AgentMessageChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(text)),
                ))
            }
            _ => continue,
        };
        let notif = SessionNotification::new(
            agent_client_protocol_schema::SessionId::new(session_id),
            update,
        );
        sender.send(notif).await?;
    }
    Ok(())
}

/// Extract plain text from a `MessageContent`.
fn extract_text(content: &PeriMessageContent) -> String {
    match content {
        PeriMessageContent::Text(s) => s.clone(),
        PeriMessageContent::Blocks(blocks) => {
            blocks
                .iter()
                .filter_map(|b| match b {
                    PeriContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

/// Abstraction over how to send a `SessionNotification`.
#[async_trait::async_trait]
pub trait ReplaySender: Send + Sync {
    async fn send(&self, notif: SessionNotification) -> Result<(), ReplayError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("transport send failed: {0}")]
    SendFailed(String),
}
```

- [ ] **Step 2: 在 `dispatch/mod.rs` 注册新模块并导出公共接口**

```rust
// 在 peri-acp/src/dispatch/mod.rs 中添加：
pub mod session_replay;

// 在 pub use 块中添加：
pub use session_replay::{replay_session_history, ReplayError, ReplaySender};
```

- [ ] **Step 3: 编译验证**

Run: `cargo check -p peri-acp`
Expected: PASS

- [ ] **Step 4: 提交**

```bash
git add peri-acp/src/dispatch/session_replay.rs peri-acp/src/dispatch/mod.rs
git commit -m "feat(acp): add session_replay helper for session/load history replay"
```

---

### Task 2: 修改 stdio `handle_load` — 回放历史后响应

**Files:**
- Modify: `peri-tui/src/acp_stdio/session/create.rs:121-182`

- [ ] **Step 1: 在 stdio handler 中接入回放逻辑**

将当前 `handle_load` 中的响应逻辑替换为：先回放历史，再响应。

```rust
/// session/load 处理器：从 ThreadStore 加载历史、回放、构建响应。
pub(crate) async fn handle_load(
    ctx: &StdioContext,
    req: LoadSessionRequest,
    responder: Responder<LoadSessionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let sid = req.session_id.0.to_string();
    let cwd = req.cwd.to_string_lossy().to_string();
    let cwd_for_skills = cwd.clone();

    ctx.session_manager.ensure_session(&sid, &cwd);
    let frozen_data = freeze::build(ctx, &cwd);

    let history = dispatch::load_session_messages(ctx.thread_store.as_ref(), &sid).await;

    // ── ACP v1 spec: replay history via session/update BEFORE responding ──
    let replay_sender = StdioReplaySender { cx: cx.clone() };
    if let Err(e) = dispatch::replay_session_history(&sid, &history, &replay_sender).await {
        tracing::warn!(session_id = %sid, error = %e, "session/load: history replay failed, continuing");
    }

    {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.get_mut(&sid) {
            if s.history.is_empty() {
                s.history = history;
            }
        } else {
            sessions.insert(
                sid.clone(),
                SessionInfo {
                    session_id: sid.clone(),
                    thread_id: sid.clone(),
                    cwd,
                    history,
                    cancel_token: None,
                    frozen: Some(frozen_data),
                    agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
                    workflow_middleware: None,
                },
            );
        }
    }

    // Respond with minimal LoadSessionResponse (history already streamed via notifications)
    let _ = responder.respond(LoadSessionResponse::new());

    commands::send_available_commands(
        &cwd_for_skills,
        &ctx.plugin_skill_roots,
        &SessionId::new(&*sid),
        &cx,
    );
    Ok(())
}
```

- [ ] **Step 2: 添加 `StdioReplaySender` 适配器到文件顶部**

在文件开头的 imports 之后添加：

```rust
use std::sync::Arc;

use peri_acp::dispatch::ReplaySender;

/// Adapts `ConnectionTo<Client>` into a `ReplaySender` for the stdio path.
struct StdioReplaySender {
    cx: ConnectionTo<Client>,
}

#[async_trait::async_trait]
impl ReplaySender for StdioReplaySender {
    async fn send(
        &self,
        notif: SessionNotification,
    ) -> Result<(), peri_acp::dispatch::ReplayError> {
        self.cx
            .send_notification(notif)
            .map_err(|e| peri_acp::dispatch::ReplayError::SendFailed(e.to_string()))
    }
}
```

需要新增的 imports（加到文件顶部）：
```rust
use agent_client_protocol::{
    schema::SessionNotification,
    Client, ConnectionTo,
};
```

- [ ] **Step 3: 编译验证**

Run: `cargo check -p peri-tui`
Expected: PASS

- [ ] **Step 4: 提交**

```bash
git add peri-tui/src/acp_stdio/session/create.rs
git commit -m "feat(acp-stdio): replay session history on session/load per ACP v1 spec"
```

---

### Task 3: 修改 TUI `handle_load` — 回放历史后响应

**Files:**
- Modify: `peri-tui/src/acp_server/requests.rs:240-303`

- [ ] **Step 1: 在 TUI handler 中接入回放逻辑**

TUI 路径的 `session/load` handler 已有 `transport: &dyn peri_acp::transport::AcpTransport`，可以直接使用。新增 `TuiReplaySender` 适配器。

在 `requests.rs` 文件顶部添加 imports：
```rust
use peri_acp::dispatch::ReplaySender;
use agent_client_protocol::schema::SessionNotification;
```

在文件底部（tests 模块之前）添加适配器：
```rust
/// Adapts `&dyn AcpTransport` into a `ReplaySender` for the TUI path.
struct TuiReplaySender<'a> {
    transport: &'a dyn peri_acp::transport::AcpTransport,
}

#[async_trait::async_trait]
impl ReplaySender for TuiReplaySender<'_> {
    async fn send(
        &self,
        notif: SessionNotification,
    ) -> Result<(), peri_acp::dispatch::ReplayError> {
        let payload = serde_json::to_value(&notif)
            .map_err(|e| peri_acp::dispatch::ReplayError::SendFailed(e.to_string()))?;
        self.transport
            .send_notification("session/update", payload)
            .await
            .map_err(|e| peri_acp::dispatch::ReplayError::SendFailed(e.to_string()))
    }
}
```

修改 `session/load` handler 中的响应部分：

```rust
        "session/load" => {
            // ... existing setup code (load history, insert into sessions) ...

            // ── ACP v1 spec: replay history via session/update BEFORE responding ──
            // Read history back from sessions (it was moved in by insert above)
            let replay_history: Vec<_> = sessions
                .get(req_session_id)
                .map(|s| s.history.clone())
                .unwrap_or_default();
            let replay_sender = TuiReplaySender { transport: transport.as_ref() };
            if let Err(e) = dispatch::replay_session_history(
                req_session_id, &replay_history, &replay_sender,
            )
            .await
            {
                tracing::warn!(session_id = %req_session_id, error = %e, "session/load: history replay failed, continuing");
            }

            // Respond with minimal LoadSessionResponse (history already streamed)
            let resp = LoadSessionResponse::new();
            // modes/configOptions already sent via send_available_commands_update
            // and send_config_option_update (see notify.rs)
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }
```

- [ ] **Step 2: 编译验证**

Run: `cargo check -p peri-tui`
Expected: PASS

- [ ] **Step 3: 提交**

```bash
git add peri-tui/src/acp_server/requests.rs
git commit -m "feat(acp-tui): replay session history on session/load per ACP v1 spec"
```

---

### Task 4: 更新 dispatch 模块导出

**Files:**
- 验证: `peri-acp/src/dispatch/mod.rs`

确认 `session_replay` 模块和 `ReplaySender`/`ReplayError` trait 已正确导出供外部 crate 使用。

- [ ] **Step 1: 运行全量测试**

```bash
cd /Users/konghayao/code/ai/perihelion
cargo test -p peri-acp --lib -- dispatch  # 验证 dispatch 模块测试
cargo test -p peri-acp                    # 全量 peri-acp 测试
cargo test -p peri-tui --lib              # peri-tui 库测试
```

Expected: ALL PASS

- [ ] **Step 2: 提交**

```bash
git add -A
git commit -m "chore: verify dispatch exports and run full test suite"
```

---

### 自审清单

**1. Spec 覆盖：**
- ✅ ACP v1 spec `session/load` 要求：回放历史 → `replay_session_history()`
- ✅ 历史回放格式：`UserMessageChunk` + `AgentMessageChunk` → 跳过 System 消息
- ✅ 响应格式：`LoadSessionResponse::new()` → `result: null/{}`
- ✅ 双路径适配：stdio (`ConnectionTo<Client>`) + TUI (`AcpTransport`)

**2. Placeholder 扫描：** 无 TBD/TODO，所有代码完整

**3. 类型一致性：**
- `replay_session_history` 签名在 Task 1 定义，Task 2/3 调用一致
- `ReplaySender` trait 在 Task 1 定义，Task 2/3 分别实现
- `SessionNotification::new(SessionId, SessionUpdate)` 构造方式与现有 `commands.rs` 一致
